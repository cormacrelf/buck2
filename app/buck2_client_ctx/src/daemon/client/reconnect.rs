/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

//! A daemon connection for a client that outlives the daemon it started with, as a long-running
//! command that sends many daemon commands does. The daemon may restart under it, as when another
//! client restarts it, and may preempt its commands.
//!
//! If another version of buck2 replaced the daemon, connecting again fails (see
//! `other_version`). Otherwise two long-running commands of different versions would each
//! keep replacing the other's daemon.
//!
//! The commands that run at the same time, such as a stream of requests and a subscription, share
//! one connection. Whichever of them finds it lost connects again, once, and the others use the
//! new connection.

use std::cell::Cell;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use buck2_cli_proto::ClientContext;
use buck2_cli_proto::new_generic::NewGenericRequest;
use buck2_cli_proto::new_generic::NewGenericResponse;
use buck2_common::invocation_paths::InvocationPaths;
use buck2_data::error::ErrorTag;

use crate::command_outcome::CommandOutcome;
use crate::daemon::client::BuckdClientConnector;
use crate::daemon::client::connect::BuckdConnectOptions;
use crate::daemon::client::connect::connect_buckd;
use crate::events_ctx::EventsCtx;

/// How long to wait before connecting to the daemon again after losing it, and, for a command
/// that retries over and over, the longest wait between attempts.
pub const RECONNECT_DELAY: Duration = Duration::from_millis(500);
pub const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// How many times in a row a request connects to the daemon again before it gives up.
pub const MAX_RECONNECTS: u32 = 3;

/// How long to wait before asking again after the daemon preempted a request.
pub const PREEMPTED_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Whether `e` says that the connection to the daemon was lost, not that the request failed.
pub fn lost_daemon(e: &buck2_error::Error) -> bool {
    e.has_tag(buck2_error::ErrorTag::ClientGrpc)
        || e.has_tag(buck2_error::ErrorTag::ClientGrpcStream)
}

/// Whether `e` says that another version of buck2 now runs the daemon. Connecting again will
/// not help, so the command should end.
pub fn other_version(e: &buck2_error::Error) -> bool {
    e.has_tag(buck2_error::ErrorTag::DaemonOtherVersion)
}

/// A connection to the daemon, which commands that run at the same time share, and which is
/// replaced when the daemon is lost. Clones are the same connection.
#[derive(Clone)]
pub struct DaemonConnection {
    inner: Rc<Inner>,
}

struct Inner {
    options: BuckdConnectOptions,
    paths: InvocationPaths,
    /// The connection, and how many times it has been replaced.
    current: RefCell<(u64, BuckdClientConnector)>,
    /// The connection that `new_generic` used last, to tell when another command replaced it.
    used: Cell<u64>,
    /// Held while connecting again, so that commands that lose the daemon together connect once.
    reconnecting: tokio::sync::Mutex<()>,
}

impl DaemonConnection {
    /// Share the connection of `buckd`. To connect again, use `options`, but leave a daemon
    /// of another version of buck2 alone.
    pub fn new(
        buckd: &BuckdClientConnector,
        mut options: BuckdConnectOptions,
        paths: InvocationPaths,
    ) -> Self {
        if let BuckdConnectOptions::Options(options) = &mut options {
            options.keep_other_version = true;
        }
        Self {
            inner: Rc::new(Inner {
                options,
                paths,
                current: RefCell::new((0, buckd.duplicate())),
                used: Cell::new(0),
                reconnecting: tokio::sync::Mutex::new(()),
            }),
        }
    }

    /// The connection now, and its number, which `reconnect` takes.
    pub fn current(&self) -> (u64, BuckdClientConnector) {
        let current = self.inner.current.borrow();
        (current.0, current.1.duplicate())
    }

    /// Replace the connection numbered `lost`, which has been lost, and return the new one. If
    /// another command has replaced it already, that is the new one, and this does not connect.
    /// Starts a daemon if there is none. Fails with an error that `other_version` knows if
    /// another version of buck2 runs the daemon now.
    pub async fn reconnect(
        &self,
        lost: u64,
        events_ctx: &mut EventsCtx,
    ) -> buck2_error::Result<(u64, BuckdClientConnector)> {
        let _reconnecting = self.inner.reconnecting.lock().await;
        if self.inner.current.borrow().0 != lost {
            return Ok(self.current());
        }
        let buckd =
            connect_buckd(self.inner.options.clone(), events_ctx, &self.inner.paths).await?;
        *self.inner.current.borrow_mut() = (lost + 1, buckd);
        Ok(self.current())
    }

    /// Run a `NewGenericRequest` as a daemon command, and return its outcome. If the connection
    /// to the daemon is lost, connect again and ask again, up to `MAX_RECONNECTS` times,
    /// calling `on_lost` first. If the daemon preempted the command for another one, ask again
    /// once it is done. `request` makes the request for each attempt.
    pub async fn new_generic(
        &self,
        context: &ClientContext,
        request: impl Fn() -> NewGenericRequest,
        events_ctx: &mut EventsCtx,
        mut on_lost: impl FnMut() -> buck2_error::Result<()>,
    ) -> buck2_error::Result<CommandOutcome<NewGenericResponse>> {
        let mut reconnects = 0;
        let (mut connection, mut buckd) = self.current();
        if self.inner.used.replace(connection) != connection {
            // Another command found the daemon lost and connected again.
            on_lost()?;
        }
        loop {
            match buckd
                .with_flushing()
                .new_generic(context.clone(), request(), events_ctx, None)
                .await
            {
                Err(e) if lost_daemon(&e) && reconnects < MAX_RECONNECTS => {
                    reconnects += 1;
                    on_lost()?;
                    tokio::time::sleep(RECONNECT_DELAY).await;
                    (connection, buckd) = self.reconnect(connection, events_ctx).await?;
                    self.inner.used.set(connection);
                }
                Err(e) => return Err(e),
                Ok(CommandOutcome::Failure(result))
                    if result
                        .get_all_errors()
                        .iter()
                        .any(|e| e.tags.contains(&(ErrorTag::DaemonPreempted as i32))) =>
                {
                    // Another command took the daemon. The daemon admits this one once that
                    // command is done.
                    tokio::time::sleep(PREEMPTED_RETRY_DELAY).await;
                }
                Ok(outcome) => return Ok(outcome),
            }
        }
    }
}
