/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

//! `buck2 dev`: run a target's `DevInfo` server, and rebuild its outputs as files change.
//!
//! The client watches the files and runs the server. Every build is a separate, short daemon
//! command, marked preemptible, so `buck2 dev` never keeps other commands waiting.
//!
//! `buck2 dev` runs only on Unix so far. Elsewhere the command only says so, and the parts that
//! do not depend on Unix are unused.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use buck2_cli_proto::ClientContext;
use buck2_cli_proto::client_context::PreemptibleWhen;
use buck2_cli_proto::new_generic::DevAction;
use buck2_cli_proto::new_generic::DevRequest;
use buck2_cli_proto::new_generic::DevResponse;
use buck2_cli_proto::new_generic::DevServer;
use buck2_cli_proto::new_generic::DevWatched;
use buck2_cli_proto::new_generic::NewGenericRequest;
use buck2_cli_proto::new_generic::NewGenericResponse;
use buck2_client_ctx::client_ctx::ClientCommandContext;
use buck2_client_ctx::command_outcome::CommandOutcome;
use buck2_client_ctx::common::BuckArgMatches;
use buck2_client_ctx::common::CommonBuildConfigurationOptions;
use buck2_client_ctx::common::CommonCommandOptions;
use buck2_client_ctx::common::CommonEventLogOptions;
use buck2_client_ctx::common::CommonStarlarkOptions;
use buck2_client_ctx::common::target_cfg::TargetCfgOptions;
use buck2_client_ctx::common::ui::CommonConsoleOptions;
use buck2_client_ctx::daemon::client::BuckdClientConnector;
use buck2_client_ctx::daemon::client::connect::BuckdConnectOptions;
use buck2_client_ctx::daemon::client::connect::connect_buckd;
use buck2_client_ctx::events_ctx::EventsCtx;
use buck2_client_ctx::exit_result::ExitResult;
use buck2_client_ctx::streaming::StreamingCommand;
use buck2_client_ctx::streaming::connect_options;
use buck2_common::invocation_paths::InvocationPaths;
use buck2_error::ErrorTag;
use buck2_error::buck2_error;
use buck2_hash::BuckMutMap;
use buck2_hash::BuckMutSet;
use notify::EventKind;
use notify::RecursiveMode;
use notify::Watcher;
use notify::event::ModifyKind;
use serde_json::json;
use tokio::sync::mpsc;

#[cfg(unix)]
use self::channel::Connection;
#[cfg(unix)]
use self::channel::Listener;
#[cfg(unix)]
use self::channel::ServerEvent;
#[cfg(unix)]
use self::process::ServerProcess;

#[cfg(unix)]
mod channel;
#[cfg(unix)]
mod process;

/// Run a target's `DevInfo` server, and rebuild as source files change.
///
/// The target must return a `DevInfo` provider. `buck2 dev` builds and starts its server, then
/// watches the source files of the target's dependency graph. When a file changes, the first
/// `DevWatch` whose files include it decides what happens: build some of the `DevInfo` outputs
/// and tell the server, rebuild and restart the server, or only tell the server.
///
/// The server may connect to the socket named in the `BUCK2_DEV_SOCKET` environment variable, and
/// exchange JSON lines with `buck2 dev` there: it gets messages such as `{"type": "built", ...}`,
/// and may send requests such as `{"type": "build", "outputs": ["name"]}`. A server that does not
/// connect is only started, and started again when its files change. See the `buck2 dev` page of
/// the rule author documentation for the messages.
///
/// If the server exits, `buck2 dev` waits for the next change and starts it again. `buck2 dev`
/// ends on Ctrl-C, once it has stopped the server, or when the user quits an interactive server.
///
/// Each build is a separate daemon command that another command may preempt, so `buck2 dev` never
/// makes other buck2 commands wait.
#[derive(Debug, clap::Parser)]
#[clap(name = "dev")]
pub struct DevCommand {
    /// The target whose `DevInfo` to run.
    #[clap(name = "TARGET")]
    target: String,

    /// After a file changes, wait until no file has changed for this many milliseconds before
    /// building.
    #[clap(long, default_value_t = 100)]
    debounce_ms: u64,

    #[clap(flatten)]
    target_cfg: TargetCfgOptions,

    #[clap(flatten)]
    common_opts: CommonCommandOptions,
}

#[async_trait(?Send)]
impl StreamingCommand for DevCommand {
    const COMMAND_NAME: &'static str = "dev";

    #[cfg(not(unix))]
    async fn exec_impl(
        self,
        _buckd: &mut BuckdClientConnector,
        _matches: BuckArgMatches<'_>,
        _ctx: &mut ClientCommandContext<'_>,
        _events_ctx: &mut EventsCtx,
    ) -> ExitResult {
        ExitResult::err(buck2_error!(
            ErrorTag::Unimplemented,
            "`buck2 dev` is not supported on Windows yet"
        ))
    }

    #[cfg(unix)]
    async fn exec_impl(
        self,
        buckd: &mut BuckdClientConnector,
        matches: BuckArgMatches<'_>,
        ctx: &mut ClientCommandContext<'_>,
        events_ctx: &mut EventsCtx,
    ) -> ExitResult {
        let mut context = ctx.client_context(matches, &self)?;
        // Another command that would wait for a `buck2 dev` build cancels it instead. `buck2 dev`
        // then retries.
        context.preemptible = PreemptibleWhen::OnDifferentState as i32;
        // `buck2 dev` handles Ctrl-C itself, so that it can stop the server and wait for it, and
        // Ctrl-Z, so that it can stop the server with itself.
        let signals = Signals::listen()?;
        let paths = ctx.paths()?.clone();
        let (server_tx, server_rx) = mpsc::unbounded_channel();
        // Listen before the server starts, so that it can connect at once.
        let listener = Listener::bind(&paths, server_tx)?;
        let daemon = Daemon {
            buckd,
            events_ctx,
            connect_options: connect_options(&self, ctx)?,
            paths,
            context,
            target: self.target.clone(),
            target_cfg: self.target_cfg.target_cfg(),
        };
        let mut session = match Session::start(
            daemon,
            Duration::from_millis(self.debounce_ms),
            listener,
            server_rx,
            signals,
        )
        .await?
        {
            Ok(session) => session,
            Err(interrupted) => return interrupted,
        };
        let result = session.run().await;
        session.shutdown().await;
        match result {
            Ok(result) => result,
            Err(e) => ExitResult::err(e),
        }
    }

    fn handles_ctrl_c(&self) -> bool {
        true
    }

    fn console_opts(&self) -> &CommonConsoleOptions {
        &self.common_opts.console_opts
    }

    fn event_log_opts(&self) -> &CommonEventLogOptions {
        &self.common_opts.event_log_opts
    }

    fn build_config_opts(&self) -> &CommonBuildConfigurationOptions {
        &self.common_opts.config_opts
    }

    fn starlark_opts(&self) -> &CommonStarlarkOptions {
        &self.common_opts.starlark_opts
    }
}

/// Sends `buck2 dev` requests to the daemon, one daemon command each.
struct Daemon<'a> {
    buckd: &'a mut BuckdClientConnector,
    events_ctx: &'a mut EventsCtx,
    /// To connect again when the daemon goes away, for example when another client restarts it.
    connect_options: BuckdConnectOptions,
    paths: InvocationPaths,
    context: ClientContext,
    target: String,
    target_cfg: buck2_cli_proto::TargetCfg,
}

/// A request the daemon ran but that failed. The console has already shown the errors.
struct Failed(Vec<String>);

impl Daemon<'_> {
    async fn request(
        &mut self,
        action: impl Fn() -> DevAction,
    ) -> buck2_error::Result<Result<DevResponse, Failed>> {
        let mut reconnects = 0;
        loop {
            let outcome = match self
                .buckd
                .with_flushing()
                .new_generic(
                    self.context.clone(),
                    NewGenericRequest::Dev(DevRequest {
                        target: self.target.clone(),
                        target_cfg: self.target_cfg.clone(),
                        action: action(),
                    }),
                    self.events_ctx,
                    None,
                )
                .await
            {
                Ok(outcome) => outcome,
                Err(e) if lost_daemon(&e) && reconnects < MAX_RECONNECTS => {
                    // The daemon went away, perhaps because another client restarted it.
                    // Connect again, starting a daemon if needed, and ask again.
                    reconnects += 1;
                    buck2_client_ctx::eprintln!(
                        "`buck2 dev` lost its connection to the daemon; connecting again"
                    )?;
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    *self.buckd =
                        connect_buckd(self.connect_options.clone(), self.events_ctx, &self.paths)
                            .await?;
                    continue;
                }
                Err(e) => return Err(e),
            };
            match outcome {
                CommandOutcome::Success(NewGenericResponse::Dev(response)) => {
                    return Ok(Ok(response));
                }
                CommandOutcome::Success(_) => {
                    return Err(buck2_error!(
                        ErrorTag::InvalidEvent,
                        "Unexpected response type from the `dev` command"
                    ));
                }
                CommandOutcome::Failure(result) => {
                    let errors = result.get_all_errors();
                    let preempted = errors.iter().any(|e| {
                        e.tags
                            .contains(&(buck2_data::error::ErrorTag::DaemonPreempted as i32))
                    });
                    if preempted {
                        // Another command took the daemon. Ask again; the daemon admits this
                        // request once that command is done.
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                    let mut messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
                    if messages.is_empty() {
                        messages.push(format!("`buck2 dev` request failed: {}", result.name()));
                    }
                    return Ok(Err(Failed(messages)));
                }
            }
        }
    }
}

fn unexpected_response() -> buck2_error::Error {
    buck2_error!(
        ErrorTag::InvalidEvent,
        "Unexpected response type from the `dev` command"
    )
}

/// How many times in a row `buck2 dev` connects to the daemon again for one request.
const MAX_RECONNECTS: u32 = 3;

/// Whether `e` says that the connection to the daemon was lost, not that the request failed.
fn lost_daemon(e: &buck2_error::Error) -> bool {
    e.has_tag(ErrorTag::ClientGrpc) || e.has_tag(ErrorTag::ClientGrpcStream)
}

/// A signal to `buck2 dev` that the session handles.
#[cfg(unix)]
enum Signal {
    /// Ctrl-C, `SIGTERM` or `SIGHUP`: stop the server and exit. Without handling these,
    /// `buck2 dev` would exit at once, and the server, in a process group of its own, would keep
    /// running.
    Quit,
    /// Ctrl-Z (`SIGTSTP`): stop the server too, then stop.
    Suspend,
    /// `SIGCONT`: the shell continued `buck2 dev`, perhaps with the terminal.
    Continue,
    /// `SIGCHLD`: the server may have stopped.
    Child,
}

#[cfg(unix)]
struct Signals {
    rx: mpsc::UnboundedReceiver<Signal>,
    task: tokio::task::JoinHandle<()>,
}

#[cfg(unix)]
impl Signals {
    fn listen() -> buck2_error::Result<Signals> {
        use tokio::signal::unix::SignalKind;
        use tokio::signal::unix::signal;

        // A signal that is ignored, such as `SIGHUP` under `nohup`, should stay ignored.
        let unless_ignored = |kind: SignalKind| -> buck2_error::Result<_> {
            Ok(if signal_ignored(kind.as_raw_value()) {
                None
            } else {
                Some(signal(kind)?)
            })
        };
        let mut interrupt = Some(signal(SignalKind::interrupt())?);
        let mut terminate = Some(signal(SignalKind::terminate())?);
        let mut hangup = unless_ignored(SignalKind::hangup())?;
        let mut stop = unless_ignored(SignalKind::from_raw(libc::SIGTSTP))?;
        let mut cont = Some(signal(SignalKind::from_raw(libc::SIGCONT))?);
        let mut child = Some(signal(SignalKind::child())?);
        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            loop {
                let signal = tokio::select! {
                    () = next(&mut interrupt) => Signal::Quit,
                    () = next(&mut terminate) => Signal::Quit,
                    () = next(&mut hangup) => Signal::Quit,
                    () = next(&mut stop) => Signal::Suspend,
                    () = next(&mut cont) => Signal::Continue,
                    () = next(&mut child) => Signal::Child,
                };
                if tx.send(signal).is_err() {
                    return;
                }
            }
        });
        Ok(Signals { rx, task })
    }

    async fn recv(&mut self) -> Signal {
        match self.rx.recv().await {
            Some(signal) => signal,
            None => futures::future::pending().await,
        }
    }

    /// Wait for `Signal::Quit`. Others are dropped: `buck2 dev` is stopping the server anyway.
    async fn quit(&mut self) {
        loop {
            if let Signal::Quit = self.recv().await {
                return;
            }
        }
    }
}

/// The next delivery of `signal`, or never.
#[cfg(unix)]
async fn next(signal: &mut Option<tokio::signal::unix::Signal>) {
    let Some(signal) = signal else {
        return futures::future::pending().await;
    };
    if signal.recv().await.is_none() {
        futures::future::pending().await
    }
}

#[cfg(unix)]
impl Drop for Signals {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(unix)]
fn signal_ignored(signal: libc::c_int) -> bool {
    // SAFETY: reads the current action into `action`, a plain C struct.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        libc::sigaction(signal, std::ptr::null(), &mut action) == 0
            && action.sa_sigaction == libc::SIG_IGN
    }
}

/// The version of the protocol between `buck2 dev` and its server, in `hello` and `ready`.
const PROTOCOL_VERSION: u64 = 1;

/// A message from the server.
#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request {
    Hello { version: u64 },
    Build { outputs: Vec<String> },
    Restart,
}

/// The running server, and its connection to `buck2 dev`, if it made one.
#[cfg(unix)]
struct Server {
    process: ServerProcess,
    argv: Vec<String>,
    interactive: bool,
    /// Connections made since the server started that have not sent `hello` yet.
    unintroduced: BuckMutMap<u64, Connection>,
    /// The connection that sent `hello`. Messages to the server go to it.
    conn: Option<(u64, Connection)>,
    /// Connections numbered below this were made to an earlier server.
    first_conn: u64,
}

#[cfg(unix)]
impl Server {
    /// Send `message` to the server. A server that has not connected gets no messages. One that
    /// has stopped reading them is disconnected, rather than have them pile up.
    fn send(&mut self, message: serde_json::Value) {
        let Some((_, connection)) = &self.conn else {
            return;
        };
        if connection.send(&message).is_err() {
            self.conn = None;
            let _ignored = buck2_client_ctx::eprintln!(
                "The `buck2 dev` server stopped reading the messages `buck2 dev` sends it, so `buck2 dev` closed its connection"
            );
        }
    }

    /// Ask the server to stop, and wait for it, killing it if it takes too long. Returns whether
    /// Ctrl-C cut the wait short.
    async fn stop(&mut self, signals: &mut Signals) -> bool {
        if self.conn.is_some() {
            self.send(json!({"type": "shutdown"}));
        }
        let sent_shutdown = self.conn.is_some();
        let interrupted = stop_process(&mut self.process, sent_shutdown, signals).await;
        self.conn = None;
        self.unintroduced.clear();
        interrupted
    }
}

/// Stop `process`, and the processes it left in its group. Ctrl-C while `buck2 dev` waits kills
/// them at once. Returns whether that happened.
#[cfg(unix)]
async fn stop_process(
    process: &mut ServerProcess,
    sent_shutdown: bool,
    signals: &mut Signals,
) -> bool {
    tokio::select! {
        () = process.stop(sent_shutdown, SERVER_STOP_TIMEOUT) => false,
        () = signals.quit() => {
            process.kill().await;
            true
        }
    }
}

/// The server, and what it sent that `buck2 dev` has not handled yet. It is apart from the rest
/// of the session so that `buck2 dev` can handle the server while a daemon request runs.
#[cfg(unix)]
struct ServerLink {
    /// `None` after the server exited, until the next change starts it again.
    server: Option<Server>,
    /// Requests from the server, handled when no daemon request is running.
    requests: VecDeque<Request>,
    /// Set when the session should end: Ctrl-C, or the user quit an interactive server.
    exit: Option<ExitResult>,
}

#[cfg(unix)]
impl ServerLink {
    fn send(&mut self, message: serde_json::Value) {
        if let Some(server) = &mut self.server {
            server.send(message);
        }
    }

    /// Handle what happened on the socket. `hello` is answered at once; requests wait in
    /// `requests`.
    fn on_event(
        &mut self,
        event: ServerEvent,
        outputs: &[String],
        pending: &BTreeSet<PathBuf>,
    ) -> buck2_error::Result<()> {
        let Some(server) = &mut self.server else {
            return Ok(());
        };
        match event {
            ServerEvent::Connected { conn, connection } => {
                if conn >= server.first_conn {
                    server.unintroduced.insert(conn, connection);
                }
            }
            ServerEvent::Closed { conn } => {
                server.unintroduced.remove(&conn);
                if server.conn.as_ref().is_some_and(|(c, _)| *c == conn) {
                    // The server keeps running; it gets no more messages.
                    server.conn = None;
                }
            }
            ServerEvent::TooLong { conn } => {
                let unintroduced = server.unintroduced.remove(&conn).is_some();
                let introduced = server.conn.as_ref().is_some_and(|(c, _)| *c == conn);
                if introduced {
                    server.conn = None;
                }
                if introduced || unintroduced {
                    buck2_client_ctx::eprintln!(
                        "The `buck2 dev` server sent a line longer than any request, so `buck2 dev` closed its connection"
                    )?;
                }
            }
            ServerEvent::Line { conn, line } => {
                let introduced = server.conn.as_ref().is_some_and(|(c, _)| *c == conn);
                if !introduced && !server.unintroduced.contains_key(&conn) {
                    // A connection to an earlier server.
                    return Ok(());
                }
                match serde_json::from_str::<Request>(&line) {
                    Ok(Request::Hello { version }) => {
                        if version != PROTOCOL_VERSION {
                            // Answer anyway: `ready` says which version `buck2 dev` speaks.
                            buck2_client_ctx::eprintln!(
                                "The `buck2 dev` server asked for protocol version {}, but `buck2 dev` speaks version {}",
                                version,
                                PROTOCOL_VERSION
                            )?;
                        }
                        // Another connection that sends `hello`, such as the real server behind a
                        // wrapper that connected first, takes over from the first.
                        if let Some(connection) = server.unintroduced.remove(&conn) {
                            server.conn = Some((conn, connection));
                        }
                        server.send(json!({
                            "type": "ready",
                            "version": PROTOCOL_VERSION,
                            "outputs": outputs,
                        }));
                        if !pending.is_empty() {
                            server.send(json!({"type": "stale", "files": pending}));
                        }
                    }
                    Ok(_) if !introduced => {
                        buck2_client_ctx::eprintln!(
                            "`buck2 dev` ignored a request from the server, which must send `hello` first: {}",
                            line
                        )?;
                    }
                    Ok(request) => self.requests.push_back(request),
                    Err(e) => {
                        buck2_client_ctx::eprintln!(
                            "`buck2 dev` ignored a request from the server: {}: {}",
                            e,
                            line
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    fn on_signal(&mut self, signal: Signal) -> buck2_error::Result<()> {
        match signal {
            Signal::Quit => self.exit = Some(ExitResult::signal_interrupt()),
            Signal::Suspend => self.suspend(),
            Signal::Continue => self.resume(),
            Signal::Child => self.on_child()?,
        }
        Ok(())
    }

    /// Ctrl-Z: stop the server's group, take the terminal back if the server had it, and stop
    /// `buck2 dev`, so that the shell sees its job stop. When the shell continues `buck2 dev`,
    /// continue the server.
    fn suspend(&mut self) {
        if let Some(server) = &mut self.server {
            server.process.pause();
        }
        stop_self();
        // Continued, by `fg` or `bg`.
        self.resume();
    }

    fn resume(&mut self) {
        if let Some(server) = &mut self.server {
            server.process.resume();
        }
    }

    /// The server may have stopped.
    fn on_child(&mut self) -> buck2_error::Result<()> {
        let Some(server) = &mut self.server else {
            return Ok(());
        };
        let Some(signal) = server.process.stopped_by() else {
            return Ok(());
        };
        let terminal_signal = signal == libc::SIGTTIN || signal == libc::SIGTTOU;
        if server.process.has_terminal() && !terminal_signal {
            // Ctrl-Z in an interactive server, which has the terminal. Stop `buck2 dev` too, so
            // that the shell gets the terminal back.
            self.suspend();
        } else if terminal_signal && !server.interactive {
            // Otherwise it waits, stopped, for a terminal it never gets.
            buck2_client_ctx::eprintln!(
                "The `buck2 dev` server tried to use the terminal, which it does not have, so `buck2 dev` stopped it. A server that reads the terminal needs `DevInfo(interactive = True)`."
            )?;
            server.process.kill_now();
        } else if terminal_signal {
            // An interactive server that read the terminal while `buck2 dev` ran in the
            // background, after `bg`. It continues when `fg` gives `buck2 dev` the terminal.
            server.process.paused_for_terminal();
        }
        Ok(())
    }

    /// The server exited. Stop what it left running in its group. The session goes on, and the
    /// next change starts the server again, unless the user quit an interactive server.
    async fn on_exit(
        &mut self,
        status: ExitStatus,
        signals: &mut Signals,
    ) -> buck2_error::Result<()> {
        let Some(mut server) = self.server.take() else {
            return Ok(());
        };
        self.requests.clear();
        if stop_process(&mut server.process, false, signals).await {
            self.exit = Some(ExitResult::signal_interrupt());
        } else if server.interactive && status.success() {
            // An interactive server has the terminal, and the user quit it.
            self.exit = Some(ExitResult::success());
        } else if server.interactive && killed_by_sigint(status) {
            // An interactive server has the terminal, so Ctrl-C reaches it and not `buck2 dev`.
            self.exit = Some(ExitResult::signal_interrupt());
        } else if status.success() {
            // A server that runs to completion, such as tests that run on each change.
            buck2_client_ctx::eprintln!(
                "The `buck2 dev` server exited. `buck2 dev` starts it again after the next change."
            )?;
        } else {
            buck2_client_ctx::eprintln!(
                "The `buck2 dev` server exited ({}). `buck2 dev` starts it again after the next change.",
                status
            )?;
        }
        Ok(())
    }
}

/// Stop `buck2 dev`, as the default action of `SIGTSTP` would. This returns when the shell
/// continues it.
#[cfg(unix)]
fn stop_self() {
    // SAFETY: no memory is passed.
    unsafe {
        libc::raise(libc::SIGSTOP);
    }
}

#[cfg(unix)]
fn killed_by_sigint(status: ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt;
    status.signal() == Some(libc::SIGINT)
}

/// Wait for the server to exit, or forever if there is none.
#[cfg(unix)]
async fn server_exited(server: &mut Option<Server>) -> ExitStatus {
    match server {
        Some(server) => server.process.wait().await,
        None => futures::future::pending().await,
    }
}

fn could_not_start(messages: Vec<String>) -> buck2_error::Error {
    buck2_error!(
        ErrorTag::Input,
        "`buck2 dev` could not start: {}",
        messages.join("\n")
    )
}

/// Where a watched path belongs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    /// A source file of a watch set.
    File(usize),
    /// A build file of a package in the graph. It `waits` when a target of the package belongs to
    /// a `notify` set.
    BuildFile { waits: bool },
    /// A `.bzl` file loaded by a package in the graph.
    Bzl,
}

/// The files of the current watch sets, and the file watcher for them.
struct Watched {
    info: DevWatched,
    owners: BuckMutMap<PathBuf, Owner>,
    /// Package directories. A file added to or removed from one may change a target.
    dirs: BuckMutSet<PathBuf>,
    _watcher: Option<notify::RecommendedWatcher>,
}

impl Watched {
    /// Nothing watched yet.
    fn empty() -> Watched {
        Watched {
            info: DevWatched {
                watch: Vec::new(),
                bzl_files: Vec::new(),
                build_files: Vec::new(),
                dirs: Vec::new(),
                incomplete: Vec::new(),
            },
            owners: BuckMutMap::default(),
            dirs: BuckMutSet::default(),
            _watcher: None,
        }
    }

    fn new(
        info: DevWatched,
        events: mpsc::UnboundedSender<notify::Result<notify::Event>>,
    ) -> buck2_error::Result<Watched> {
        let mut owners = BuckMutMap::default();
        for (i, set) in info.watch.iter().enumerate() {
            for f in &set.files {
                owners.insert(PathBuf::from(f), Owner::File(i));
            }
        }
        // A build file changes the graph, even when a target also uses it as a source file.
        for f in &info.build_files {
            owners.insert(PathBuf::from(&f.path), Owner::BuildFile { waits: f.waits });
        }
        for f in &info.bzl_files {
            owners.entry(PathBuf::from(f)).or_insert(Owner::Bzl);
        }
        let dirs: BuckMutSet<PathBuf> = info.dirs.iter().map(PathBuf::from).collect();
        let mut watch_dirs = BTreeSet::new();
        for path in owners.keys() {
            if let Some(parent) = path.parent() {
                watch_dirs.insert(parent.to_path_buf());
            }
        }
        watch_dirs.extend(dirs.iter().cloned());

        let mut watcher = notify::recommended_watcher(move |event| {
            let _ignored = events.send(event);
        })
        .map_err(|e| buck2_error!(ErrorTag::Environment, "Failed to start watching files: {e}"))?;
        for dir in &watch_dirs {
            if dir.is_dir() {
                watcher
                    .watch(dir, RecursiveMode::NonRecursive)
                    .map_err(|e| {
                        buck2_error!(
                            ErrorTag::Environment,
                            "Failed to watch `{}`: {e}",
                            dir.display()
                        )
                    })?;
            }
        }
        Ok(Watched {
            info,
            owners,
            dirs,
            _watcher: Some(watcher),
        })
    }

    /// Whether `path` is inside a package directory, so that adding or removing it may change a
    /// target.
    fn in_package(&self, path: &Path) -> bool {
        path.ancestors().skip(1).any(|dir| self.dirs.contains(dir))
    }

    fn has_notify(&self) -> bool {
        self.info.watch.iter().any(|set| set.notify)
    }

    /// Whether a change to `path` waits until the server asks for a build: a file of a
    /// `notify` set, a build file of a package with a target in a `notify` set, or a `.bzl` file,
    /// which may change any target, when there is a `notify` set.
    fn waits(&self, path: &Path) -> bool {
        match self.owners.get(path) {
            Some(Owner::File(i)) => self.info.watch[*i].notify,
            Some(Owner::BuildFile { waits }) => *waits,
            Some(Owner::Bzl) => self.has_notify(),
            None => false,
        }
    }

    /// The files whose content `buck2 dev` compares with `Session::baseline`.
    fn baseline_files(&self) -> Vec<PathBuf> {
        self.owners
            .keys()
            .filter(|path| self.waits(path))
            .cloned()
            .collect()
    }

    /// The watch sets whose targets are different in `new`: after a build file changed, the sets
    /// whose targets it changed.
    fn changed_sets(&self, new: &DevWatched) -> BTreeSet<usize> {
        if self.info.watch.len() != new.watch.len() {
            return (0..new.watch.len()).collect();
        }
        self.info
            .watch
            .iter()
            .zip(&new.watch)
            .enumerate()
            .filter(|(_, (old, new))| old.targets_digest != new.targets_digest)
            .map(|(i, _)| i)
            .collect()
    }
}

/// How long `buck2 dev` waits for a server to exit after `shutdown`, and again after `SIGTERM`.
const SERVER_STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// The longest `buck2 dev` waits for files to stop changing before it builds.
const MAX_DEBOUNCE: Duration = Duration::from_secs(2);

/// The content of a file that a `notify` set or the graph uses, when the server last asked for a
/// build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Baseline {
    /// The file's hash, or `None` if it could not be read.
    Hash(Option<blake3::Hash>),
    /// The file changed before it was hashed, so its earlier content is not known.
    Unknown,
}

type Hashes = Arc<Mutex<BuckMutMap<PathBuf, Baseline>>>;

fn hash_file(path: &Path) -> Option<blake3::Hash> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut file, &mut hasher).ok()?;
    Some(hasher.finalize())
}

/// A changed path, as `buck2 dev` found it.
struct FileState {
    path: PathBuf,
    /// The content hash, or `None` if it could not be read.
    hash: Option<blake3::Hash>,
    exists: bool,
}

/// Hash `paths` on a blocking thread, so that a large file does not hold up the session.
async fn read_files(paths: Vec<PathBuf>) -> Vec<FileState> {
    tokio::task::spawn_blocking(move || {
        paths
            .into_iter()
            .map(|path| FileState {
                hash: hash_file(&path),
                exists: path.symlink_metadata().is_ok(),
                path,
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// A path that no watch set or graph file owns, and that does not exist now, was created and
/// removed again since `buck2 dev` last looked, such as a temporary file of an editor: vim saves
/// by first creating and removing `4913` to test the directory. It cannot have changed a target.
fn passing(watched: &Watched, file: &FileState) -> bool {
    !file.exists && !watched.owners.contains_key(&file.path)
}

/// Hash `files` on another thread and record them in `hashes`, keeping entries already there:
/// those of files that changed since, or that the server has built.
fn hash_in_background(files: Vec<PathBuf>, hashes: Hashes) {
    std::thread::spawn(move || {
        for path in files {
            let hash = hash_file(&path);
            if let Ok(mut hashes) = hashes.lock() {
                hashes.entry(path).or_insert(Baseline::Hash(hash));
            }
        }
    });
}

#[cfg(unix)]
struct Session<'a> {
    daemon: Daemon<'a>,
    debounce: Duration,
    events_tx: mpsc::UnboundedSender<notify::Result<notify::Event>>,
    events_rx: mpsc::UnboundedReceiver<notify::Result<notify::Event>>,
    watched: Watched,
    /// The server's command line and the names of the outputs, from the last analysis.
    server_info: DevServer,
    /// The socket the server connects to, and what happens on it.
    listener: Listener,
    server_rx: mpsc::UnboundedReceiver<ServerEvent>,
    link: ServerLink,
    signals: Signals,
    generation: u64,
    /// The digest of each output when it was last built.
    digests: BuckMutMap<String, Option<String>>,
    /// The content of each file of a `notify` set, and of each `.bzl` file of the graph, when
    /// the server last asked for a build.
    baseline: Hashes,
    /// The content of each changed file when it was last handled.
    last_seen: BuckMutMap<PathBuf, Option<blake3::Hash>>,
    /// Files of `notify` sets, and `.bzl` files if there is a `notify` set, that differ from
    /// `baseline`.
    pending: BTreeSet<PathBuf>,
    /// Watch sets with changes that were not built yet, because `pending` is not empty or
    /// because analysing the target again was interrupted, and the files that changed in them.
    held: BTreeMap<usize, Vec<PathBuf>>,
    /// Changed build files, and files added to or removed from packages. The next analysis says
    /// which watch sets they changed.
    held_graph_paths: Vec<PathBuf>,
    /// Analyse the target again before building: the graph changed, or the last analysis
    /// failed, was interrupted, or could not find all the files to watch.
    resolve_due: bool,
    /// The graph changed since the server was last sent `graph_changed`.
    graph_dirty: bool,
    /// Start the server again at the next analysis: a restart did not finish.
    restart_due: bool,
    /// Changed paths that `buck2 dev` read during a request. They are handled next.
    queued: BTreeSet<PathBuf>,
    /// The outputs and files of a build that was cancelled because a file changed. The next
    /// build builds them too.
    cancelled_outputs: BTreeSet<String>,
    cancelled_files: BTreeSet<PathBuf>,
}

/// What became of a request that `buck2 dev` read file events during.
enum Outcome {
    Done(DevResponse),
    /// The daemon ran the request, but it failed. The console has already shown the errors.
    Failed(Vec<String>),
    /// A file changed that makes the request out of date, so `buck2 dev` cancelled it.
    Interrupted,
}

/// What became of analysing the target again.
enum Resolved {
    /// The watch sets whose targets changed.
    Done(BTreeSet<usize>),
    Failed,
    Interrupted,
}

#[cfg(unix)]
impl<'a> Session<'a> {
    /// Analyse the target and start the server. `Err` is the result of Ctrl-C before the server
    /// started.
    async fn start(
        mut daemon: Daemon<'a>,
        debounce: Duration,
        listener: Listener,
        server_rx: mpsc::UnboundedReceiver<ServerEvent>,
        mut signals: Signals,
    ) -> buck2_error::Result<Result<Session<'a>, ExitResult>> {
        let response = {
            let request = daemon.request(|| DevAction::Server);
            tokio::pin!(request);
            loop {
                tokio::select! {
                    response = &mut request => break response?,
                    signal = signals.recv() => match signal {
                        Signal::Quit => return Ok(Err(ExitResult::signal_interrupt())),
                        Signal::Suspend => stop_self(),
                        Signal::Continue | Signal::Child => {}
                    },
                }
            }
        };
        let server_info = match response {
            Ok(DevResponse::Server(info)) => info,
            Ok(_) => return Err(unexpected_response()),
            Err(Failed(messages)) => return Err(could_not_start(messages)),
        };
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let mut session = Session {
            daemon,
            debounce,
            events_tx,
            events_rx,
            watched: Watched::empty(),
            server_info,
            listener,
            server_rx,
            link: ServerLink {
                server: None,
                requests: VecDeque::new(),
                exit: None,
            },
            signals,
            generation: 0,
            digests: BuckMutMap::default(),
            baseline: Arc::new(Mutex::new(BuckMutMap::default())),
            last_seen: BuckMutMap::default(),
            pending: BTreeSet::new(),
            held: BTreeMap::new(),
            held_graph_paths: Vec::new(),
            resolve_due: false,
            graph_dirty: false,
            restart_due: false,
            queued: BTreeSet::new(),
            cancelled_outputs: BTreeSet::new(),
            cancelled_files: BTreeSet::new(),
        };
        session.spawn().await?;
        Ok(Ok(session))
    }

    async fn run(&mut self) -> buck2_error::Result<ExitResult> {
        // The server starts while `buck2 dev` finds the files to watch, which may build what
        // dynamic actions need. It handles the server's requests afterwards.
        match self.request(|| DevAction::Watch).await? {
            Outcome::Done(DevResponse::Watch(watched)) => self.install_watch(watched)?,
            Outcome::Done(_) => return Err(unexpected_response()),
            Outcome::Failed(messages) => return Err(could_not_start(messages)),
            // Nothing is watched yet, so only the end of the session interrupts this.
            Outcome::Interrupted => {}
        }
        loop {
            if let Some(exit) = self.link.exit.take() {
                return Ok(exit);
            }
            if let Some(request) = self.link.requests.pop_front() {
                self.on_request(request).await?;
                continue;
            }
            if !self.queued.is_empty() {
                // Files changed during the last build.
                let paths = std::mem::take(&mut self.queued);
                let paths = self.debounce(paths).await;
                self.on_changes(paths).await?;
                continue;
            }
            tokio::select! {
                event = self.events_rx.recv() => {
                    let Some(event) = event else { continue };
                    let mut paths = BTreeSet::new();
                    collect_paths(&self.watched, event, &mut paths);
                    if paths.is_empty() {
                        continue;
                    }
                    let paths = self.debounce(paths).await;
                    self.on_changes(paths).await?;
                }
                Some(event) = self.server_rx.recv() => {
                    self.link.on_event(event, &self.server_info.outputs, &self.pending)?;
                }
                status = server_exited(&mut self.link.server) => {
                    self.link.on_exit(status, &mut self.signals).await?;
                }
                signal = self.signals.recv() => self.link.on_signal(signal)?,
            }
        }
    }

    /// Stop the server before `buck2 dev` exits. Another Ctrl-C kills it at once.
    async fn shutdown(&mut self) {
        if let Some(mut server) = self.link.server.take() {
            server.stop(&mut self.signals).await;
        }
    }

    /// Add the paths of further events to `paths` until the watched files stop changing, but
    /// for no longer than `MAX_DEBOUNCE`, in case they never do.
    async fn debounce(&mut self, mut paths: BTreeSet<PathBuf>) -> BTreeSet<PathBuf> {
        let deadline = tokio::time::Instant::now() + MAX_DEBOUNCE;
        let mut quiet_until = tokio::time::Instant::now() + self.debounce;
        loop {
            let wait_until = quiet_until.min(deadline);
            match tokio::time::timeout_at(wait_until, self.events_rx.recv()).await {
                Ok(Some(event)) => {
                    let before = paths.len();
                    collect_paths(&self.watched, event, &mut paths);
                    if paths.len() != before {
                        quiet_until = tokio::time::Instant::now() + self.debounce;
                    }
                }
                _ => return paths,
            }
        }
    }

    async fn on_request(&mut self, request: Request) -> buck2_error::Result<()> {
        match request {
            Request::Build { outputs } => {
                if self.resolve_due {
                    self.reresolve().await?;
                }
                self.build(outputs, Vec::new()).await?;
                self.reset_pending().await;
            }
            Request::Restart => {
                self.restart().await?;
                self.reset_pending().await;
            }
            // Answered as soon as it arrives.
            Request::Hello { .. } => {}
        }
        Ok(())
    }

    /// The server asked for a build or restart, so it now uses the files as they are: they are
    /// the new baseline.
    async fn reset_pending(&mut self) {
        let files = read_files(self.pending.iter().cloned().collect()).await;
        if let Ok(mut baseline) = self.baseline.lock() {
            for file in files {
                baseline.insert(file.path, Baseline::Hash(file.hash));
            }
        }
        self.held.clear();
        self.held_graph_paths.clear();
        if !self.pending.is_empty() {
            self.pending.clear();
            self.link
                .send(json!({"type": "stale", "files": Vec::<String>::new()}));
        }
    }

    async fn on_changes(&mut self, paths: BTreeSet<PathBuf>) -> buck2_error::Result<()> {
        let mut changed_sets: BTreeMap<usize, Vec<PathBuf>> = BTreeMap::new();
        let mut waiting: Vec<(PathBuf, Option<blake3::Hash>)> = Vec::new();
        let mut bzl_changed: Vec<PathBuf> = Vec::new();
        let mut graph_changed = false;

        let paths = paths
            .into_iter()
            .filter(|path| self.watched.owners.contains_key(path) || self.watched.in_package(path))
            .collect();
        for file in read_files(paths).await {
            if passing(&self.watched, &file) {
                continue;
            }
            let FileState { path, hash, .. } = file;
            let owner = self.watched.owners.get(&path).copied();
            if self.watched.waits(&path) {
                waiting.push((path.clone(), hash));
            }
            if let Some(Owner::File(i)) = owner {
                if self.watched.info.watch[i].notify {
                    continue;
                }
            }
            if self.last_seen.get(&path) == Some(&hash) {
                continue;
            }
            self.last_seen.insert(path.clone(), hash);
            match owner {
                Some(Owner::File(i)) => changed_sets.entry(i).or_default().push(path),
                Some(Owner::Bzl) => {
                    graph_changed = true;
                    bzl_changed.push(path);
                }
                // A build file, or a file added to or removed from a package. Analysing the
                // target again says which watch sets' targets it changed.
                Some(Owner::BuildFile { .. }) | None => {
                    graph_changed = true;
                    self.held_graph_paths.push(path);
                }
            }
        }
        self.graph_dirty |= graph_changed;
        self.resolve_due |= graph_changed;

        // Track the files that differ from what the server last built.
        let before = self.pending.clone();
        for (path, hash) in waiting {
            let baseline = match self.baseline.lock() {
                // A file not hashed yet changed before `buck2 dev` saw its old content.
                Ok(mut baseline) => *baseline.entry(path.clone()).or_insert(Baseline::Unknown),
                Err(_) => Baseline::Unknown,
            };
            if baseline == Baseline::Hash(hash) {
                self.pending.remove(&path);
            } else {
                self.pending.insert(path);
            }
        }
        if self.pending != before {
            self.link
                .send(json!({"type": "stale", "files": self.pending}));
        }

        // A changed `.bzl` file may change any target, so it counts as a change to every set
        // that builds. For `notify` sets it is in `pending`.
        if !bzl_changed.is_empty() {
            for (i, set) in self.watched.info.watch.iter().enumerate() {
                if !set.notify {
                    changed_sets
                        .entry(i)
                        .or_default()
                        .extend(bzl_changed.iter().cloned());
                }
            }
        }
        for (set, files) in changed_sets {
            self.held.entry(set).or_default().extend(files);
        }

        if !self.pending.is_empty() {
            // Building now would build the changed files of the `notify` sets too, and so would
            // analysing, which builds the server. Wait until the server asks for a build, or
            // the files go back to what they were.
            return Ok(());
        }
        if self.resolve_due {
            match self.reresolve().await? {
                Resolved::Done(sets) => {
                    let paths = std::mem::take(&mut self.held_graph_paths);
                    for set in sets {
                        self.held
                            .entry(set)
                            .or_default()
                            .extend(paths.iter().cloned());
                    }
                }
                Resolved::Failed => {}
                // The changes read meanwhile are handled next, and analyse again.
                Resolved::Interrupted => return Ok(()),
            }
        }
        let all = std::mem::take(&mut self.held);
        let sets: BTreeSet<usize> = all.keys().copied().collect();
        let files: BTreeSet<PathBuf> = all.into_values().flatten().collect();
        self.apply(sets, files.into_iter().collect()).await
    }

    /// Do what the watch sets `sets` ask for after `files` changed.
    async fn apply(
        &mut self,
        sets: BTreeSet<usize>,
        files: Vec<PathBuf>,
    ) -> buck2_error::Result<()> {
        let mut outputs = BTreeSet::new();
        let mut restart = false;
        for i in sets {
            let Some(set) = self.watched.info.watch.get(i) else {
                continue;
            };
            outputs.extend(set.build.iter().cloned());
            restart |= set.restart;
        }
        if self.link.server.is_none()
            && (restart || !outputs.is_empty() || !self.cancelled_outputs.is_empty())
        {
            // The server exited. Start it again, built from the files as they are now. It asks
            // for the outputs it needs.
            return self.restart().await;
        }
        if !outputs.is_empty() || !self.cancelled_outputs.is_empty() {
            self.build(outputs.into_iter().collect(), files).await?;
        }
        if restart {
            self.restart().await?;
        }
        Ok(())
    }

    /// Send a request to the daemon, and read file events and the server while it runs. If a file
    /// changes that makes the request out of date, cancel it: dropping the request cancels it in
    /// the daemon, and the next request, which sees different files, preempts it if it has not
    /// stopped yet. The session ending cancels it too.
    async fn request(&mut self, action: impl Fn() -> DevAction) -> buck2_error::Result<Outcome> {
        if self.link.exit.is_some() {
            return Ok(Outcome::Interrupted);
        }
        let Session {
            daemon,
            events_rx,
            watched,
            last_seen,
            queued,
            server_info,
            server_rx,
            link,
            pending,
            signals,
            ..
        } = &mut *self;
        let request = daemon.request(action);
        tokio::pin!(request);
        loop {
            tokio::select! {
                result = &mut request => {
                    return Ok(match result? {
                        Ok(response) => Outcome::Done(response),
                        Err(Failed(messages)) => Outcome::Failed(messages),
                    });
                }
                // The server is not kept waiting while the daemon works. Its requests are handled
                // after this one.
                Some(event) = server_rx.recv() => {
                    link.on_event(event, &server_info.outputs, pending)?;
                }
                status = server_exited(&mut link.server) => {
                    link.on_exit(status, signals).await?;
                    if link.exit.is_some() {
                        return Ok(Outcome::Interrupted);
                    }
                }
                signal = signals.recv() => {
                    // Ctrl-Z stops `buck2 dev` here, and the daemon request with it.
                    link.on_signal(signal)?;
                    if link.exit.is_some() {
                        return Ok(Outcome::Interrupted);
                    }
                }
                event = events_rx.recv() => {
                    let Some(event) = event else { continue };
                    let mut paths = BTreeSet::new();
                    collect_paths(watched, event, &mut paths);
                    let interrupt = interrupts(watched, last_seen, &paths).await;
                    queued.append(&mut paths);
                    if interrupt {
                        return Ok(Outcome::Interrupted);
                    }
                }
            }
        }
    }

    /// Build `outputs`, and the outputs of a cancelled build. If a file changes that makes the
    /// build out of date, cancel it: the next build, with the new files, builds its outputs.
    async fn build(
        &mut self,
        outputs: Vec<String>,
        files: Vec<PathBuf>,
    ) -> buck2_error::Result<()> {
        let mut outputs: BTreeSet<String> = outputs.into_iter().collect();
        outputs.append(&mut self.cancelled_outputs);
        let outputs: Vec<String> = outputs.into_iter().collect();
        let mut files: BTreeSet<PathBuf> = files.into_iter().collect();
        files.append(&mut self.cancelled_files);
        self.generation += 1;
        let generation = self.generation;
        self.link.send(json!({
            "type": "building",
            "gen": generation,
            "outputs": outputs,
            "files": files,
        }));

        let outcome = self
            .request(|| DevAction::Build {
                outputs: outputs.clone(),
            })
            .await?;
        match outcome {
            Outcome::Done(DevResponse::Build(built)) => {
                let mut message = serde_json::Map::new();
                for (name, output) in built.outputs {
                    let changed =
                        self.digests.get(&name) != Some(&output.digest) || output.digest.is_none();
                    self.digests.insert(name.clone(), output.digest.clone());
                    message.insert(
                        name,
                        json!({
                            "path": output.path,
                            "digest": output.digest,
                            "changed": changed,
                        }),
                    );
                }
                self.link
                    .send(json!({"type": "built", "gen": generation, "outputs": message}));
            }
            Outcome::Done(_) => return Err(unexpected_response()),
            Outcome::Failed(messages) => {
                self.link.send(json!({
                    "type": "failed",
                    "gen": generation,
                    "error": messages.join("\n"),
                }));
            }
            Outcome::Interrupted => {
                self.cancelled_outputs.extend(outputs);
                self.cancelled_files.extend(files);
                self.link
                    .send(json!({"type": "cancelled", "gen": generation}));
            }
        }
        Ok(())
    }

    /// Analyse the target again: build the server, and start it again if a restart is due or
    /// its command line changed; then find the files to watch again. A file change interrupts
    /// it like a build. If it fails or is interrupted, `buck2 dev` analyses again after the next
    /// change.
    async fn reresolve(&mut self) -> buck2_error::Result<Resolved> {
        self.resolve_due = true;
        let info = match self.request(|| DevAction::Server).await? {
            Outcome::Done(DevResponse::Server(info)) => info,
            Outcome::Done(_) => return Err(unexpected_response()),
            Outcome::Failed(messages) => {
                self.send_failed(messages);
                return Ok(Resolved::Failed);
            }
            Outcome::Interrupted => return Ok(Resolved::Interrupted),
        };
        // Start the server again if a restart is due, it exited, or it would start differently.
        let restart = std::mem::take(&mut self.restart_due)
            || self.link.server.as_ref().is_none_or(|server| {
                server.argv != info.server || server.interactive != info.interactive
            });
        self.server_info = info;
        if restart {
            self.respawn().await?;
        }
        let watched = match self.request(|| DevAction::Watch).await? {
            Outcome::Done(DevResponse::Watch(watched)) => watched,
            Outcome::Done(_) => return Err(unexpected_response()),
            Outcome::Failed(messages) => {
                self.send_failed(messages);
                return Ok(Resolved::Failed);
            }
            Outcome::Interrupted => return Ok(Resolved::Interrupted),
        };
        let changed = self.watched.changed_sets(&watched);
        self.install_watch(watched)?;
        if std::mem::take(&mut self.graph_dirty) {
            self.link.send(json!({"type": "graph_changed"}));
        }
        Ok(Resolved::Done(changed))
    }

    /// Watch the files of `info`. If it could not find all of them, analyse again after the
    /// next change.
    fn install_watch(&mut self, info: DevWatched) -> buck2_error::Result<()> {
        for message in &info.incomplete {
            buck2_client_ctx::eprintln!(
                "`buck2 dev` could not find every file to watch, so it watches more files, and \
                 looks again after the next change: {}",
                message
            )?;
        }
        self.resolve_due = !info.incomplete.is_empty();
        self.watched = Watched::new(info, self.events_tx.clone())?;
        hash_in_background(self.watched.baseline_files(), self.baseline.clone());
        Ok(())
    }

    fn send_failed(&mut self, messages: Vec<String>) {
        self.generation += 1;
        self.link.send(json!({
            "type": "failed",
            "gen": self.generation,
            "error": messages.join("\n"),
        }));
    }

    /// Rebuild the server and start it again. If that fails or is interrupted, the next analysis
    /// starts it again.
    async fn restart(&mut self) -> buck2_error::Result<()> {
        self.restart_due = true;
        self.reresolve().await?;
        Ok(())
    }

    /// Stop the server, if it is running, and start it again. If it does not start, the next
    /// change that builds or restarts tries again.
    async fn respawn(&mut self) -> buck2_error::Result<()> {
        // Stop the old server first, so that the new one can use its resources, such as a port.
        if let Some(mut server) = self.link.server.take() {
            if server.stop(&mut self.signals).await {
                self.link.exit = Some(ExitResult::signal_interrupt());
                return Ok(());
            }
        }
        // The new server asks for what it needs.
        self.link.requests.clear();
        self.digests.clear();
        self.cancelled_outputs.clear();
        self.cancelled_files.clear();
        if let Err(e) = self.spawn().await {
            buck2_client_ctx::eprintln!(
                "{:#}\n`buck2 dev` starts the server again after the next change.",
                e
            )?;
        }
        Ok(())
    }

    /// Start the server. It gets `ready` once it connects and sends `hello`.
    async fn spawn(&mut self) -> buck2_error::Result<()> {
        // The server writes to the terminal too. The superconsole, which redraws the lines at
        // the bottom of the terminal, would draw over its output, so from now on `buck2 dev`
        // uses the simple console. Ending the first daemon command has usually done this already.
        self.daemon.events_ctx.erase_interactive_output().await?;
        let first_conn = self.listener.next_conn();
        let process = ServerProcess::spawn(
            &self.server_info.server,
            &self.server_info.project_root,
            self.server_info.interactive,
            self.listener.address(),
        )?;
        self.link.server = Some(Server {
            process,
            argv: self.server_info.server.clone(),
            interactive: self.server_info.interactive,
            unintroduced: BuckMutMap::default(),
            conn: None,
            first_conn,
        });
        Ok(())
    }
}

/// Add the paths of `event` that belong to a watch set, a package or the graph.
fn collect_paths(
    watched: &Watched,
    event: notify::Result<notify::Event>,
    paths: &mut BTreeSet<PathBuf>,
) {
    let Ok(event) = event else { return };
    if matches!(event.kind, EventKind::Access(_)) {
        return;
    }
    // A file that no watch set owns matters only when it is added to or removed from a package.
    // Writing to one, such as an editor's swap file, cannot change a target.
    let only_content = matches!(
        event.kind,
        EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Metadata(_))
    );
    for path in event.paths {
        if watched.owners.contains_key(&path) || (!only_content && watched.in_package(&path)) {
            paths.insert(path);
        }
    }
}

/// Whether a change to one of `paths` makes the running request out of date, so that
/// `buck2 dev` should cancel it and send it again: the file has new content, and a change to it
/// does not wait for the server.
async fn interrupts(
    watched: &Watched,
    last_seen: &BuckMutMap<PathBuf, Option<blake3::Hash>>,
    paths: &BTreeSet<PathBuf>,
) -> bool {
    let paths = paths
        .iter()
        .filter(|path| !watched.waits(path))
        .cloned()
        .collect();
    read_files(paths)
        .await
        .iter()
        .any(|file| !passing(watched, file) && last_seen.get(&file.path) != Some(&file.hash))
}
