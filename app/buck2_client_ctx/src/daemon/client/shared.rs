/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

//! A daemon connection shared by the commands of a long-running client that run at the same
//! time, such as a stream of requests and a subscription.
//!
//! The connection is not replaced when the daemon goes away. A daemon is usually stopped on
//! purpose, by `buck2 kill`, `buck2 clean` or a different buck2 version, and a client that
//! started it again and carried on building would undo that. Such a client should end instead;
//! `lost_daemon` tells it when.

use std::rc::Rc;
use std::time::Duration;

use buck2_cli_proto::ClientContext;
use buck2_cli_proto::new_generic::NewGenericPartialResult;
use buck2_cli_proto::new_generic::NewGenericRequest;
use buck2_cli_proto::new_generic::NewGenericResponse;
use buck2_data::error::ErrorTag;

use crate::command_outcome::CommandOutcome;
use crate::daemon::client::BuckdClientConnector;
use crate::events_ctx::EventsCtx;

/// How long to wait before asking again after the daemon preempted a request.
pub const PREEMPTED_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Whether `e` says that the connection to the daemon was lost, not that the request failed.
pub fn lost_daemon(e: &buck2_error::Error) -> bool {
    e.has_tag(buck2_error::ErrorTag::ClientGrpc)
        || e.has_tag(buck2_error::ErrorTag::ClientGrpcStream)
}

/// A connection to the daemon that commands running at the same time share. Clones are the
/// same connection.
#[derive(Clone)]
pub struct DaemonConnection {
    buckd: Rc<BuckdClientConnector>,
}

impl DaemonConnection {
    /// Share the connection of `buckd`.
    pub fn new(buckd: &BuckdClientConnector) -> Self {
        Self {
            buckd: Rc::new(buckd.duplicate()),
        }
    }

    /// Another handle on the connection.
    pub fn connector(&self) -> BuckdClientConnector {
        self.buckd.duplicate()
    }

    /// Run a `NewGenericRequest` as a daemon command, and return its outcome. If the daemon
    /// preempted the command for another one, ask again once that one is done. `request` makes
    /// the request for each attempt. Partial results go to `on_partial_result`, those of an
    /// attempt that was preempted too. Fails with an error that `lost_daemon` recognises if the
    /// daemon goes away.
    pub async fn new_generic(
        &self,
        context: &ClientContext,
        request: impl Fn() -> NewGenericRequest,
        events_ctx: &mut EventsCtx,
        on_partial_result: &mut (
                 dyn FnMut(NewGenericPartialResult) -> buck2_error::Result<()> + Send
             ),
    ) -> buck2_error::Result<CommandOutcome<NewGenericResponse>> {
        let mut buckd = self.connector();
        loop {
            match buckd
                .with_flushing()
                .new_generic_with_partial_results(
                    context.clone(),
                    request(),
                    events_ctx,
                    None,
                    on_partial_result,
                )
                .await?
            {
                CommandOutcome::Failure(result)
                    if result
                        .get_all_errors()
                        .iter()
                        .any(|e| e.tags.contains(&(ErrorTag::DaemonPreempted as i32))) =>
                {
                    // Another command took the daemon. The daemon admits this one once that
                    // command is done.
                    tokio::time::sleep(PREEMPTED_RETRY_DELAY).await;
                }
                outcome => return Ok(outcome),
            }
        }
    }
}
