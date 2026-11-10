/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

//! The changes to the project's files, as the daemon's file watcher sees them.
//!
//! `buck2 dev` subscribes to them on a connection of its own, which runs for the whole session
//! next to its daemon commands. The subscription takes no lock in the daemon, so it neither waits
//! for other commands nor holds them up. Its events go to an `EventsCtx` without subscribers, so
//! that its start and end do not show in the console or the event log.

use std::collections::VecDeque;
use std::time::Duration;

use async_trait::async_trait;
use buck2_cli_proto::ClientContext;
use buck2_cli_proto::SubscriptionResponseWrapper;
use buck2_cli_proto::client_context::PreemptibleWhen;
use buck2_client_ctx::command_outcome::CommandOutcome;
use buck2_client_ctx::daemon::client::connect::BuckdConnectOptions;
use buck2_client_ctx::daemon::client::connect::connect_buckd;
use buck2_client_ctx::events_ctx::EventsCtx;
use buck2_client_ctx::events_ctx::PartialResultCtx;
use buck2_client_ctx::events_ctx::PartialResultHandler;
use buck2_common::invocation_paths::InvocationPaths;
use buck2_error::ErrorTag;
use buck2_error::buck2_error;
use buck2_subscription_proto::subscription_response::Response;
use buck2_wrapper_common::invocation_id::TraceId;
use futures::FutureExt;
use futures::StreamExt;
use futures::future::LocalBoxFuture;
use tokio::sync::mpsc;

/// How long `buck2 dev` waits before it subscribes again after the subscription ended, as when
/// the daemon restarted, and the longest it waits between attempts.
const RESUBSCRIBE_DELAY: Duration = Duration::from_millis(500);
const MAX_RESUBSCRIBE_DELAY: Duration = Duration::from_secs(5);

/// A change to a project-relative path. The daemon says what it means.
pub(super) enum FileChange {
    Path(String),
    /// Changes were missed: anything may have changed.
    Missed,
}

type Subscription = LocalBoxFuture<'static, buck2_error::Result<CommandOutcome<()>>>;

pub(super) struct FileChanges {
    connect: Connect,
    received: mpsc::UnboundedReceiver<buck2_subscription_proto::FileChanges>,
    sender: mpsc::UnboundedSender<buck2_subscription_proto::FileChanges>,
    subscription: Subscription,
    /// The subscription ended, and its replacement has not said that it is in place.
    resubscribing: bool,
    delay: Duration,
    buffered: VecDeque<FileChange>,
}

/// What a subscription needs to connect to the daemon.
#[derive(Clone)]
struct Connect {
    options: BuckdConnectOptions,
    paths: InvocationPaths,
    context: ClientContext,
}

impl FileChanges {
    /// Subscribe, and wait until the subscription is in place: every change from then on is
    /// reported. Fails if the daemon's file watcher cannot report changes as they happen.
    pub(super) async fn subscribe(
        options: BuckdConnectOptions,
        paths: InvocationPaths,
        context: &ClientContext,
    ) -> buck2_error::Result<FileChanges> {
        let mut context = context.clone();
        // A command of its own, which the daemon need not preempt.
        context.trace_id = TraceId::new().to_string();
        context.preemptible = PreemptibleWhen::Never as i32;
        let connect = Connect {
            options,
            paths,
            context,
        };
        let (sender, mut received) = mpsc::unbounded_channel();
        let mut subscription = subscription(connect.clone(), sender.clone(), Duration::ZERO);
        tokio::select! {
            // The first message says that the subscription is in place.
            Some(_) = received.recv() => {}
            result = &mut subscription => {
                return Err(match result {
                    Ok(CommandOutcome::Failure(result)) => {
                        let messages: Vec<String> =
                            result.get_all_errors().into_iter().map(|e| e.message).collect();
                        buck2_error!(
                            ErrorTag::Input,
                            "`buck2 dev` needs the daemon to report file changes as they happen: {}",
                            messages.join("\n")
                        )
                    }
                    Ok(CommandOutcome::Success(())) => buck2_error!(
                        ErrorTag::Tier0,
                        "The daemon ended the `buck2 dev` file change subscription"
                    ),
                    Err(e) => e,
                });
            }
        }
        Ok(FileChanges {
            connect,
            received,
            sender,
            subscription,
            resubscribing: false,
            delay: RESUBSCRIBE_DELAY,
            buffered: VecDeque::new(),
        })
    }

    /// The next change. Cancelling this loses nothing.
    pub(super) async fn next(&mut self) -> FileChange {
        loop {
            if let Some(change) = self.buffered.pop_front() {
                return change;
            }
            tokio::select! {
                Some(changes) = self.received.recv() => self.receive(changes),
                result = &mut self.subscription => {
                    // The stream ended, as when the daemon restarted. Subscribe again; what changed
                    // meanwhile is unknown, which the new subscription reports once it is in place.
                    if let Err(e) = result {
                        tracing::debug!("`buck2 dev` lost its file change subscription: {e:#}");
                    }
                    self.resubscribing = true;
                    self.subscription =
                        subscription(self.connect.clone(), self.sender.clone(), self.delay);
                    self.delay = (self.delay * 2).min(MAX_RESUBSCRIBE_DELAY);
                }
            }
        }
    }

    fn receive(&mut self, changes: buck2_subscription_proto::FileChanges) {
        if changes.missed || self.resubscribing {
            self.resubscribing = false;
            self.delay = RESUBSCRIBE_DELAY;
            self.buffered.push_back(FileChange::Missed);
        }
        for change in changes.changes {
            self.buffered.push_back(FileChange::Path(change.path));
        }
    }
}

/// Connect, and subscribe to the file changes, after `delay`.
fn subscription(
    connect: Connect,
    sender: mpsc::UnboundedSender<buck2_subscription_proto::FileChanges>,
    delay: Duration,
) -> Subscription {
    async move {
        tokio::time::sleep(delay).await;
        // No subscribers: the subscription's own events are of no interest.
        let mut events_ctx = EventsCtx::new(None, Vec::new());
        let mut buckd = connect_buckd(connect.options, &mut events_ctx, &connect.paths).await?;
        let request = buck2_cli_proto::SubscriptionRequestWrapper {
            request: Some(buck2_subscription_proto::SubscriptionRequest {
                request: Some(buck2_subscription_proto::SubscribeToFileChanges {}.into()),
            }),
        };
        // The stream stays open until `buck2 dev` drops the subscription.
        let requests = futures::stream::once(futures::future::ready(request))
            .chain(futures::stream::pending());
        let outcome = buckd
            .with_flushing()
            .subscription(
                connect.context,
                requests,
                &mut events_ctx,
                &mut Handler { sender },
            )
            .await?;
        Ok(match outcome {
            CommandOutcome::Success(_) => CommandOutcome::Success(()),
            CommandOutcome::Failure(result) => CommandOutcome::Failure(result),
        })
    }
    .boxed_local()
}

struct Handler {
    sender: mpsc::UnboundedSender<buck2_subscription_proto::FileChanges>,
}

#[async_trait]
impl PartialResultHandler for Handler {
    type PartialResult = SubscriptionResponseWrapper;

    async fn handle_partial_result(
        &mut self,
        _ctx: PartialResultCtx<'_>,
        result: Self::PartialResult,
    ) -> buck2_error::Result<()> {
        if let Some(Response::FileChanges(changes)) = result.response.and_then(|r| r.response) {
            // `buck2 dev` is exiting if this fails.
            let _ignored = self.sender.send(changes);
        }
        Ok(())
    }
}
