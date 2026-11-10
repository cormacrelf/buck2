/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

use std::time::Duration;

use buck2_error::BuckErrorOptionContext;
use buck2_error::ErrorTag;
use buck2_error::buck2_error;
use buck2_events::dispatch::span_async;
use buck2_file_watcher::file_watcher::ChangeKind;
use buck2_file_watcher::file_watcher::FileChangeSubscription;
use buck2_file_watcher::file_watcher::PushedChanges;
use buck2_server_ctx::commands::command_end;
use buck2_server_ctx::ctx::ServerCommandContextTrait;
use buck2_server_ctx::dev_interest::InterestSubscription;
use buck2_server_ctx::partial_result_dispatcher::PartialResultDispatcher;
use buck2_server_ctx::streaming_request_handler::StreamingRequestHandler;
use futures::future::FutureExt;
use tokio::time::MissedTickBehavior;

use crate::active_commands;
use crate::ctx::ServerCommandContext;

/// The next changes of a file watcher subscription that may matter to the session that
/// `interest` is of, if it is of one. Never without a subscription.
async fn next_file_changes(
    subscription: &mut Option<FileChangeSubscription>,
    interest: Option<&InterestSubscription>,
) -> PushedChanges {
    loop {
        let Some(changes) = subscription.as_mut() else {
            return futures::future::pending().await;
        };
        let Some(changes) = changes.next().await else {
            // The file watcher is gone, as when the daemon shuts down.
            *subscription = None;
            return PushedChanges {
                changes: Vec::new(),
                missed: true,
            };
        };
        let mut changes = (*changes).clone();
        if let Some(interest) = interest.and_then(|interest| interest.interest()) {
            changes
                .changes
                .retain(|(path, _)| interest.may_matter(path));
            if changes.changes.is_empty() && !changes.missed {
                continue;
            }
        }
        return changes;
    }
}

fn file_changes_response(changes: PushedChanges) -> buck2_subscription_proto::FileChanges {
    buck2_subscription_proto::FileChanges {
        changes: changes
            .changes
            .into_iter()
            .map(|(path, kind)| buck2_subscription_proto::FileChange {
                path: path.to_string(),
                kind: match kind {
                    ChangeKind::Contents => buck2_subscription_proto::FileChangeKind::Contents,
                    ChangeKind::FileAddedOrRemoved => {
                        buck2_subscription_proto::FileChangeKind::FileAddedOrRemoved
                    }
                    ChangeKind::DirAddedOrRemoved => {
                        buck2_subscription_proto::FileChangeKind::DirAddedOrRemoved
                    }
                } as i32,
            })
            .collect(),
        missed: changes.missed,
    }
}

pub(crate) async fn run_subscription_server_command(
    ctx: &ServerCommandContext<'_>,
    mut partial_result_dispatcher: PartialResultDispatcher<
        buck2_cli_proto::SubscriptionResponseWrapper,
    >,
    mut req: StreamingRequestHandler<buck2_cli_proto::SubscriptionRequestWrapper>,
) -> buck2_error::Result<buck2_cli_proto::SubscriptionCommandResponse> {
    let start_event = ctx
        .command_start_event(buck2_data::SubscriptionCommandStart {}.into())
        .await?;
    span_async(start_event, async move {
        let result: buck2_error::Result<buck2_cli_proto::SubscriptionCommandResponse> = try {
            let mut wants_active_commands = false;
            // This command takes no lock on the daemon's state, so it neither waits for other
            // commands nor holds them up.
            let mut file_changes: Option<FileChangeSubscription> = None;
            let mut interest: Option<InterestSubscription> = None;

            let mut ticker = tokio::time::interval(Duration::from_millis(100));
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

            let disconnect = loop {
                futures::select! {
                    message = req.message().fuse() => {
                        use buck2_subscription_proto::subscription_request::Request;

                        let message = message?.request.internal_error("Empty subscription message");
                        let request = message?.request.ok_or_else(|| {
                            buck2_error!(
                                ErrorTag::SubscriptionEmptyRequest,
                                "Empty subscription request"
                            )
                        })?;
                        match request {
                            Request::Disconnect(disconnect) => {
                                break disconnect;
                            }
                            Request::SubscribeToActiveCommands(buck2_subscription_proto::SubscribeToActiveCommands {}) => {
                                wants_active_commands = true;
                            }
                            Request::SubscribeToFileChanges(buck2_subscription_proto::SubscribeToFileChanges { session }) => {
                                if !session.is_empty() && interest.is_none() {
                                    interest = Some(InterestSubscription::new(session));
                                }
                                if file_changes.is_none() {
                                    file_changes =
                                        Some(ctx.base_context.repo().file_watcher.subscribe().await?);
                                }
                                // Every change from here on is sent.
                                partial_result_dispatcher.emit(buck2_cli_proto::SubscriptionResponseWrapper {
                                    response: Some(buck2_subscription_proto::SubscriptionResponse {
                                        response: Some(buck2_subscription_proto::FileChanges::default().into())
                                    })
                                });
                            }
                        }
                    }
                    changes = next_file_changes(&mut file_changes, interest.as_ref()).fuse() => {
                        partial_result_dispatcher.emit(buck2_cli_proto::SubscriptionResponseWrapper {
                            response: Some(buck2_subscription_proto::SubscriptionResponse {
                                response: Some(file_changes_response(changes).into())
                            })
                        });
                    }
                    _ = ticker.tick().fuse() => {
                        if wants_active_commands {
                            let snapshot = buck2_subscription_proto::ActiveCommandsSnapshot {
                                active_commands: active_commands::active_commands_snapshot(),
                            };
                            partial_result_dispatcher.emit(buck2_cli_proto::SubscriptionResponseWrapper {
                                response: Some(buck2_subscription_proto::SubscriptionResponse {
                                    response: Some(snapshot.into())
                                })
                            });
                        }
                    }
                }
            };

            partial_result_dispatcher.emit(buck2_cli_proto::SubscriptionResponseWrapper {
                response: Some(buck2_subscription_proto::SubscriptionResponse {
                    response: Some(buck2_subscription_proto::Goodbye {
                        reason: disconnect.reason,
                        ok: disconnect.ok,
                    }.into())
                })
            });

            buck2_cli_proto::SubscriptionCommandResponse {}
        };

        let end_event = command_end(&result, buck2_data::SubscriptionCommandEnd {});
        (result, end_event)
    })
    .await
}
