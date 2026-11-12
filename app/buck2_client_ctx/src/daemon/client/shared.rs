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
//! time, such as a `buck2 dev` session and its file change subscription.
//!
//! The connection is not replaced when the daemon goes away. A daemon is usually stopped on
//! purpose, by `buck2 kill`, `buck2 clean` or a different buck2 version, and a client that
//! started it again and carried on building would undo that. Such a client should end instead;
//! `lost_daemon` tells it when.

use std::rc::Rc;

use buck2_cli_proto::ClientContext;
use buck2_cli_proto::new_generic::DevSessionRequest;
use buck2_cli_proto::new_generic::NewGenericPartialResult;
use futures::Stream;

use crate::command_outcome::CommandOutcome;
use crate::daemon::client::BuckdClientConnector;
use crate::events_ctx::EventsCtx;

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

    /// Run a `buck2 dev` session, which lasts until `requests` ends. Partial results go to
    /// `on_partial_result`. Fails with an error that `lost_daemon` recognises if the daemon goes
    /// away.
    pub async fn dev_session(
        &self,
        context: &ClientContext,
        requests: impl Stream<Item = DevSessionRequest> + Send + Sync + 'static,
        events_ctx: &mut EventsCtx,
        on_partial_result: &mut (
                 dyn FnMut(NewGenericPartialResult) -> buck2_error::Result<()> + Send
             ),
    ) -> buck2_error::Result<CommandOutcome<()>> {
        let mut buckd = self.connector();
        buckd
            .with_flushing()
            .dev_session(context.clone(), requests, events_ctx, on_partial_result)
            .await
    }
}
