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
//! The client acts on the file changes that the daemon's file watcher reports to it, and runs the
//! server. Every build is a separate, short daemon command, marked preemptible, so `buck2 dev`
//! never keeps other commands waiting.
//!
//! `buck2 dev` runs only on Unix so far. Elsewhere the command only says so, and the parts that
//! do not depend on Unix are unused.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::VecDeque;
use std::path::Path;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use async_trait::async_trait;
use buck2_cli_proto::ClientContext;
use buck2_cli_proto::client_context::PreemptibleWhen;
use buck2_cli_proto::new_generic::DevAction;
use buck2_cli_proto::new_generic::DevBuilt;
use buck2_cli_proto::new_generic::DevBuiltOutput;
use buck2_cli_proto::new_generic::DevChange;
use buck2_cli_proto::new_generic::DevRequest;
use buck2_cli_proto::new_generic::DevResponse;
use buck2_cli_proto::new_generic::DevServer;
use buck2_cli_proto::new_generic::DevWatchRequest;
use buck2_cli_proto::new_generic::DevWatchState;
use buck2_cli_proto::new_generic::DevWatched;
use buck2_cli_proto::new_generic::NewGenericPartialResult;
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
use buck2_client_ctx::daemon::client::shared::DaemonConnection;
use buck2_client_ctx::daemon::client::shared::lost_daemon;
use buck2_client_ctx::events_ctx::EventsCtx;
use buck2_client_ctx::exit_result::ExitResult;
use buck2_client_ctx::streaming::StreamingCommand;
use buck2_client_ctx::subscribers::subscriber::SessionUpdate;
use buck2_error::ErrorTag;
use buck2_error::buck2_error;
use buck2_error::internal_error;
use buck2_hash::BuckMutMap;
use buck2_wrapper_common::invocation_id::TraceId;
use serde_json::json;
use tokio::sync::mpsc;

#[cfg(unix)]
use self::changes::FileChange;
#[cfg(unix)]
use self::changes::FileChanges;
#[cfg(unix)]
use self::channel::Connection;
#[cfg(unix)]
use self::channel::Listener;
#[cfg(unix)]
use self::channel::ServerEvent;
#[cfg(unix)]
use self::process::ServerProcess;

#[cfg(unix)]
mod changes;
#[cfg(unix)]
mod channel;
#[cfg(unix)]
mod process;
#[cfg(unix)]
mod pty;

/// Run a target's `DevInfo` server, and rebuild as source files change.
///
/// The target must return a `DevInfo` provider, or a `RunInfo` provider, which is then the
/// server. `buck2 dev` builds and starts its server, then watches the source files of the
/// target's dependency graph. It builds `DevInfo` outputs when the server asks for them. When a
/// file changes, it tells the server which of the outputs that it asked for are stale, and
/// starts the server again if its command changed.
///
/// The server may connect to the socket named in the `BUCK2_DEV_SOCKET` environment variable, and
/// exchange JSON lines with `buck2 dev` there: it gets messages such as `{"type": "built", ...}`,
/// and may send requests such as `{"type": "build", "outputs": ["name"]}`. A server that does not
/// connect is only started, and started again when its command changes. See the `buck2 dev` page
/// of the rule author documentation for the messages.
///
/// If the server exits, `buck2 dev` waits for the next change and starts it again. `buck2 dev`
/// ends on Ctrl-C, once it has stopped the server, or when the user quits an interactive server.
///
/// Each build is a separate daemon command that another command may preempt, so `buck2 dev` never
/// makes other buck2 commands wait.
///
/// In place of a target, `buck2 dev` takes a bxl function, `path/to/file.bxl:name`, that returns a
/// list of providers, as a rule does: `buck2 dev file.bxl:name -- BXL_ARGS -- SERVER_ARGS`.
#[derive(Debug, clap::Parser)]
#[clap(name = "dev")]
pub struct DevCommand {
    /// The target whose `DevInfo` to run, or a bxl function, `path/to/file.bxl:name`, that
    /// returns the providers to use in place of a target's.
    #[clap(name = "TARGET")]
    target: String,

    /// Arguments after `--` are appended to the server's command line, for the whole session.
    /// For a bxl function, they are the function's arguments, up to a second `--`, and those
    /// after it are the server's.
    #[clap(name = "ARGS", last = true)]
    args: Vec<String>,

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
        let connection = DaemonConnection::new(buckd);
        self.run(connection, matches, ctx, events_ctx).await
    }

    fn handles_ctrl_c(&self) -> bool {
        true
    }

    fn runs_many_commands(&self) -> bool {
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

#[cfg(unix)]
impl DevCommand {
    async fn run(
        self,
        connection: DaemonConnection,
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
        // What the console shows between and during daemon commands: the server's output, the
        // status line, and Ctrl-Z, which takes the canvas off the terminal.
        let (updates_tx, updates) = mpsc::unbounded_channel();
        events_ctx.set_session_updates(updates);
        let console = Console {
            updates: updates_tx,
            canvas: Arc::new(AtomicBool::new(events_ctx.used_superconsole)),
        };
        let paths = ctx.paths()?.clone();
        let (server_tx, server_rx) = mpsc::unbounded_channel();
        // Listen before the server starts, so that it can connect at once.
        let listener = Listener::bind(&paths, server_tx)?;
        // The daemon's file watcher reports the changes to the project's files. Subscribe before
        // anything else, so that `buck2 dev` stops at once if it cannot.
        let session = TraceId::new().to_string();
        let changes = FileChanges::subscribe(connection.clone(), &context, session.clone()).await?;
        let (partials_tx, partials) = mpsc::unbounded_channel();
        let (bxl_args, server_args) = split_args(&self.target, &self.args);
        let daemon = Daemon {
            connection,
            events_ctx,
            console: console.clone(),
            context,
            session,
            target: self.target.clone(),
            bxl_args,
            target_cfg: self.target_cfg.target_cfg(),
            partials: partials_tx,
        };
        let mut session = match Session::start(
            daemon,
            partials,
            server_args,
            Duration::from_millis(self.debounce_ms),
            listener,
            server_rx,
            signals,
            console,
            changes,
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
}

/// Whether `target` names a bxl function, `path/to/file.bxl:name`, rather than a target.
fn is_bxl_function(target: &str) -> bool {
    target
        .rsplit_once(':')
        .is_some_and(|(path, _)| path.ends_with(".bxl"))
}

/// The arguments after `--`, split into those of the bxl function, if `target` is one, and those
/// of the server. A bxl function takes those up to a second `--`, and the server those after it.
fn split_args(target: &str, args: &[String]) -> (Option<Vec<String>>, Vec<String>) {
    if !is_bxl_function(target) {
        return (None, args.to_vec());
    }
    match args.iter().position(|arg| arg == "--") {
        Some(i) => (Some(args[..i].to_vec()), args[i + 1..].to_vec()),
        None => (Some(args.to_vec()), Vec::new()),
    }
}

/// Sends `buck2 dev` requests to the daemon, one daemon command each.
struct Daemon<'a> {
    /// The connection to the daemon, shared with the file change subscription. When the daemon
    /// goes away, `buck2 dev` ends: it does not start the daemon again.
    connection: DaemonConnection,
    events_ctx: &'a mut EventsCtx,
    console: Console,
    context: ClientContext,
    /// The name of this session, which its file change subscription gave the daemon too.
    session: String,
    target: String,
    /// When `target` is a bxl function, its arguments.
    bxl_args: Option<Vec<String>>,
    target_cfg: buck2_cli_proto::TargetCfg,
    /// Where the results of a streaming build go before its response.
    partials: mpsc::UnboundedSender<DevBuilt>,
}

/// A request the daemon ran but that failed. The console has already shown the errors.
struct Failed(Vec<String>);

impl Daemon<'_> {
    async fn request(
        &mut self,
        action: impl Fn() -> DevAction,
    ) -> buck2_error::Result<Result<DevResponse, Failed>> {
        let partials = self.partials.clone();
        let outcome = self
            .connection
            .new_generic(
                &self.context,
                || {
                    NewGenericRequest::Dev(DevRequest {
                        target: self.target.clone(),
                        bxl_args: self.bxl_args.clone(),
                        target_cfg: self.target_cfg.clone(),
                        action: action(),
                        session: self.session.clone(),
                    })
                },
                self.events_ctx,
                &mut |partial| {
                    match partial {
                        // `buck2 dev` is exiting if this fails.
                        NewGenericPartialResult::Dev(built) => {
                            let _ignored = partials.send(built);
                        }
                    }
                    Ok(())
                },
            )
            .await
            .map_err(|e| if lost_daemon(&e) { daemon_gone(e) } else { e })?;
        match outcome {
            CommandOutcome::Success(NewGenericResponse::Dev(response)) => Ok(Ok(response)),
            CommandOutcome::Success(_) => Err(buck2_error!(
                ErrorTag::InvalidEvent,
                "Unexpected response type from the `dev` command"
            )),
            CommandOutcome::Failure(result) => {
                let mut messages: Vec<String> = result
                    .get_all_errors()
                    .into_iter()
                    .map(|e| e.message)
                    .collect();
                if messages.is_empty() {
                    messages.push(format!("`buck2 dev` request failed: {}", result.name()));
                }
                Ok(Err(Failed(messages)))
            }
        }
    }
}

/// Say why `buck2 dev` ends when it loses the daemon.
fn daemon_gone(e: buck2_error::Error) -> buck2_error::Error {
    e.context(
        "The buck2 daemon went away, so `buck2 dev` is exiting. It does not start the daemon again",
    )
}

fn unexpected_response() -> buck2_error::Error {
    buck2_error!(
        ErrorTag::InvalidEvent,
        "Unexpected response type from the `dev` command"
    )
}

/// Where `buck2 dev` shows its own messages: above the superconsole's canvas while there is one,
/// so that the canvas does not draw over them, and otherwise on stderr.
#[derive(Clone)]
struct Console {
    /// To the console. See `EventsCtx::set_session_updates`.
    updates: mpsc::UnboundedSender<SessionUpdate>,
    /// Whether the superconsole draws a canvas.
    canvas: Arc<AtomicBool>,
}

impl Console {
    fn message(&self, message: impl AsRef<str>) -> buck2_error::Result<()> {
        let message = message.as_ref();
        if self.has_canvas() {
            self.send(SessionUpdate::Output(
                message.lines().map(str::to_owned).collect(),
            ));
            Ok(())
        } else {
            buck2_client_ctx::eprintln!("{}", message)
        }
    }

    fn send(&self, update: SessionUpdate) {
        // The console is gone only once `buck2 dev` is exiting.
        let _ignored = self.updates.send(update);
    }

    fn has_canvas(&self) -> bool {
        self.canvas.load(Ordering::Relaxed)
    }

    fn drop_canvas(&self) {
        self.canvas.store(false, Ordering::Relaxed);
    }
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
    /// `SIGWINCH`: the terminal changed size.
    Resize,
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
        let mut resize = Some(signal(SignalKind::window_change())?);
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
                    () = next(&mut resize) => Signal::Resize,
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

/// The versions of the protocol between `buck2 dev` and its server that `buck2 dev` speaks. A
/// server names one in `hello`; `ready` repeats it. `hello` and `unsupported` keep their shape in
/// every version, so that a server can find one that both sides speak.
const PROTOCOL_VERSIONS: &[u64] = &[1];

/// A message from the server.
#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request {
    Hello {
        version: u64,
    },
    Build {
        outputs: Vec<String>,
        /// Send the result of each output as soon as it is known, rather than all of them at
        /// the end.
        #[serde(default)]
        streaming: bool,
        /// The outputs stay stale until the server says with `ack` that it has them.
        #[serde(default)]
        stale_until_ack: bool,
    },
    /// The server is up to date with `outputs` as build `gen` left them.
    Ack {
        r#gen: u64,
        outputs: Vec<String>,
    },
    Restart,
    /// For tests only, and only with `BUCK2_DEV_TEST_SYNC` set: answered with
    /// `{"type": "_synced", "id": id}` once `buck2 dev` has handled the requests before it and
    /// the file changes it has heard of, so that a test can tell that nothing more is coming
    /// without waiting for a while.
    #[serde(rename = "_sync")]
    Sync {
        id: u64,
    },
}

/// Whether the server may send `_sync`. See `Request::Sync`.
fn test_sync_enabled() -> bool {
    buck2_core::buck2_env!("BUCK2_DEV_TEST_SYNC", bool, applicability = testing).unwrap_or(false)
}

/// An output that the server asked for, with the changes since the server last got it.
#[derive(Default)]
struct Tracked {
    /// The server has a build of the output.
    received: bool,
    /// The changes since then, by project-relative path, each with the scopes that hold it and
    /// the number of the last build that had started when `buck2 dev` heard of it. The empty
    /// path stands for the project: changes were missed, so any file may have changed. `None`
    /// is a change to the targets that names no file.
    changes: BTreeMap<Option<String>, (u64, BTreeSet<String>)>,
}

impl Tracked {
    /// Add `change`, heard of after build `generation` started.
    fn add(&mut self, change: DevChange, generation: u64) {
        let scopes: BTreeSet<String> = change.scopes.into_iter().collect();
        match self.changes.get_mut(&change.path) {
            Some((heard, within)) => {
                *heard = generation.max(*heard);
                *within = within.intersection(&scopes).cloned().collect();
            }
            None => {
                self.changes.insert(change.path, (generation, scopes));
            }
        }
    }

    /// The project-relative paths that changed.
    fn files(&self) -> impl Iterator<Item = &String> {
        self.changes.keys().flatten()
    }

    /// The scopes that hold every change, or `None` if nothing changed.
    fn within(&self) -> Option<BTreeSet<String>> {
        let mut scopes = self.changes.values().map(|(_, scopes)| scopes);
        let first = scopes.next()?.clone();
        Some(scopes.fold(first, |within, scopes| {
            within.intersection(scopes).cloned().collect()
        }))
    }

    /// The server has the output as build `generation` left it: with every change heard of
    /// before that build started.
    fn received(&mut self, generation: u64) {
        self.received = true;
        self.changes.retain(|_, (heard, _)| *heard >= generation);
    }
}

/// The `stale` message for the outputs in `tracked` that changed, if any did. `root` is the
/// project root, which paths are relative to.
fn stale_message(tracked: &BTreeMap<String, Tracked>, root: &str) -> Option<serde_json::Value> {
    let mut outputs = serde_json::Map::new();
    for (name, tracked) in tracked {
        let Some(within) = tracked.within() else {
            continue;
        };
        // Within no scope is known for an output that the server does not have.
        let within: Vec<String> = if tracked.received {
            within.into_iter().collect()
        } else {
            Vec::new()
        };
        let files: Vec<String> = tracked.files().map(|path| absolute(root, path)).collect();
        outputs.insert(name.clone(), json!({"within": within, "files": files}));
    }
    (!outputs.is_empty()).then(|| json!({"type": "stale", "outputs": outputs}))
}

/// The absolute path of the project-relative `path`. The empty path is the project root.
fn absolute(root: &str, path: &str) -> String {
    if path.is_empty() {
        return root.to_owned();
    }
    Path::new(root).join(path).to_string_lossy().into_owned()
}

/// The running server, and its connection to `buck2 dev`, if it made one.
#[cfg(unix)]
struct Server {
    process: ServerProcess,
    argv: Vec<String>,
    interactive: bool,
    /// The digest of what the command line names, as it was when the server started.
    digest: String,
    /// The environment variables that the local resources gave the server.
    env: Vec<(String, String)>,
    /// The local resources that ran when the server started. See `DevServer.resources`.
    resources: Vec<String>,
    /// Connections made since the server started that have not sent `hello` yet.
    unintroduced: BuckMutMap<u64, Connection>,
    /// The connection that sent `hello`. Messages to the server go to it.
    conn: Option<(u64, Connection)>,
    /// Connections numbered below this were made to an earlier server.
    first_conn: u64,
    /// The build whose `building` the connection got, and whose result it has not. A connection
    /// gets the result of a build only if it got its `building`.
    announced: Option<u64>,
    console: Console,
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
            let _ignored = self.console.message("The `buck2 dev` server stopped reading the messages `buck2 dev` sends it, so `buck2 dev` closed its connection");
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
    console: Console,
}

#[cfg(unix)]
impl ServerLink {
    fn send(&mut self, message: serde_json::Value) {
        if let Some(server) = &mut self.server {
            server.send(message);
        }
    }

    /// Send `building` for build `generation`, to a server that is connected now.
    fn send_building(&mut self, generation: u64, message: serde_json::Value) {
        if let Some(server) = &mut self.server {
            if server.conn.is_some() {
                server.send(message);
                server.announced = server.conn.is_some().then_some(generation);
            }
        }
    }

    /// Send a result of build `generation`: `built`, `failed` or `cancelled`. Only a
    /// connection that got the build's `building` gets it, so that a server that connected
    /// during a build is not told about a build it did not hear start. `last` is false for a
    /// `built` that more results of the build follow. Returns whether the server got it.
    fn send_result(&mut self, generation: u64, message: serde_json::Value, last: bool) -> bool {
        if let Some(server) = &mut self.server {
            if server.announced == Some(generation) {
                if last {
                    server.announced = None;
                }
                server.send(message);
                return server.conn.is_some();
            }
        }
        false
    }

    /// Handle what happened on the socket. `hello` is answered at once; requests wait in
    /// `requests`.
    fn on_event(
        &mut self,
        event: ServerEvent,
        info: &DevServer,
        stale: Option<serde_json::Value>,
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
                    self.console.message("The `buck2 dev` server sent a line longer than any request, so `buck2 dev` closed its connection")?;
                }
            }
            ServerEvent::Line { conn, line } => {
                let introduced = server.conn.as_ref().is_some_and(|(c, _)| *c == conn);
                if !introduced && !server.unintroduced.contains_key(&conn) {
                    // A connection to an earlier server.
                    return Ok(());
                }
                match serde_json::from_str::<Request>(&line) {
                    Ok(Request::Hello { version }) if !PROTOCOL_VERSIONS.contains(&version) => {
                        self.console.message(format!(
                            "The `buck2 dev` server asked for protocol version {}, but `buck2 dev` speaks {}",
                            version,
                            PROTOCOL_VERSIONS
                                .iter()
                                .map(|v| format!("version {v}"))
                                .collect::<Vec<_>>()
                                .join(" and ")
                        ))?;
                        // The connection stays open, unintroduced, so that it can send `hello`
                        // again with a version from the list. One that had sent `hello` before
                        // gets nothing more until it does.
                        if introduced {
                            if let Some((conn, connection)) = server.conn.take() {
                                server.announced = None;
                                server.unintroduced.insert(conn, connection);
                            }
                        }
                        if let Some(connection) = server.unintroduced.get(&conn) {
                            let _ignored = connection.send(&json!({
                                "type": "unsupported",
                                "versions": PROTOCOL_VERSIONS,
                            }));
                        }
                    }
                    Ok(Request::Hello { version }) => {
                        // Another connection that sends `hello`, such as the real server behind a
                        // wrapper that connected first, takes over from the first.
                        if let Some(connection) = server.unintroduced.remove(&conn) {
                            server.conn = Some((conn, connection));
                            server.announced = None;
                        }
                        server.send(json!({
                            "type": "ready",
                            "version": version,
                            "outputs": info.outputs,
                            "scopes": info.scopes,
                        }));
                        if let Some(stale) = stale {
                            server.send(stale);
                        }
                    }
                    Ok(Request::Sync { .. }) if !test_sync_enabled() => {
                        self.console.message(format!(
                            "`buck2 dev` ignored a request from the server: {}",
                            line
                        ))?;
                    }
                    Ok(_) if !introduced => {
                        self.console.message(format!(
                            "`buck2 dev` ignored a request from the server, which must send `hello` first: {}",
                            line
                        ))?;
                    }
                    Ok(request) => self.requests.push_back(request),
                    Err(e) => {
                        self.console.message(format!(
                            "`buck2 dev` ignored a request from the server: {}: {}",
                            e, line
                        ))?;
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
            Signal::Resize => {
                if let Some(server) = &self.server {
                    server.process.resize();
                }
            }
        }
        Ok(())
    }

    /// Ctrl-Z: stop the server's group, take the terminal back if the server had it, and stop
    /// `buck2 dev`, so that the shell sees its job stop. The console stops `buck2 dev`, once it
    /// has taken its canvas off the terminal. When the shell continues `buck2 dev`, `SIGCONT`
    /// continues the server.
    fn suspend(&mut self) {
        if let Some(server) = &mut self.server {
            server.process.pause();
        }
        self.console.send(SessionUpdate::Stop);
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
            self.console.message("The `buck2 dev` server tried to use the terminal, which it does not have, so `buck2 dev` stopped it. A server that reads the terminal needs `DevInfo(interactive = True)`.")?;
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
            self.console.message(
                "The `buck2 dev` server exited. `buck2 dev` starts it again after the next change.",
            )?;
        } else {
            self.console.message(format!(
                "The `buck2 dev` server exited ({}). `buck2 dev` starts it again after the next change.",
                status
            ))?;
        }
        Ok(())
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

/// What the daemon last said about the targets of the server and the outputs.
#[derive(Default)]
struct Watched {
    /// What `buck2 dev` last acted on, to send back with the next `Watch`, which then says what
    /// changed since. `None` before the first answer.
    state: Option<DevWatchState>,
    /// The `incomplete` messages last shown.
    incomplete: Vec<String>,
    /// How many targets the server and the outputs may be built from.
    targets: usize,
}

/// How long `buck2 dev` waits for a server to exit after `shutdown`, and again after `SIGTERM`.
const SERVER_STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// The longest `buck2 dev` waits for files to stop changing before it builds.
const MAX_DEBOUNCE: Duration = Duration::from_secs(2);

#[cfg(unix)]
struct Session<'a> {
    daemon: Daemon<'a>,
    /// The arguments after `--`, appended to the server's command line.
    server_args: Vec<String>,
    debounce: Duration,
    /// The changes the daemon's file watcher reports.
    changes: FileChanges,
    /// The file watcher missed changes, and `buck2 dev` has not acted on that yet: anything may
    /// have changed.
    missed: bool,
    /// Missed changes arrived during a request. They are handled next, once: if asking the daemon
    /// fails, the next change asks again.
    missed_queued: bool,
    watched: Watched,
    /// The server's command line and the names of the outputs, from the last analysis.
    server_info: DevServer,
    /// The socket the server connects to, and what happens on it.
    listener: Listener,
    server_rx: mpsc::UnboundedReceiver<ServerEvent>,
    link: ServerLink,
    signals: Signals,
    /// The status line last sent to the console.
    status: String,
    /// How the last build went, and how long it took.
    last_build: Option<(&'static str, Duration)>,
    generation: u64,
    /// The results of a streaming build, which arrive before its response.
    partials: mpsc::UnboundedReceiver<DevBuilt>,
    /// The build that runs, if one does.
    building: Option<Building>,
    delivered: Delivered,
    /// The outputs that the running server asked for, by name.
    tracked: BTreeMap<String, Tracked>,
    /// The outputs of builds that the daemon stopped since `buck2 dev` last asked it about
    /// changes, each with the paths that stopped them. The empty path stands for the project.
    /// The next answer makes each of them stale, even if it says nothing of them.
    stopped: BTreeMap<String, BTreeSet<String>>,
    /// Project-relative paths that changed since `buck2 dev` last acted on changes. Each `Watch`
    /// asks about all of them.
    unacted: BTreeSet<String>,
    /// Analyse the target again before building: the graph changed, or the last analysis
    /// failed.
    resolve_due: bool,
    /// The graph changed since the server was last sent `graph_changed`.
    graph_dirty: bool,
    /// Start the server again at the next analysis: a restart did not finish.
    restart_due: bool,
    /// The last `Watch` failed, so the analysis of the target failed: the next `Watch` that
    /// works must analyse again, and tell the server.
    watch_failed: bool,
    /// The errors of the last `Watch` that failed, to send them only once.
    watch_failure: Option<Vec<String>>,
    /// The errors of the last analysis or build of the server that failed, to send them only
    /// once for changes that are nothing to the server or the outputs.
    resolve_failure: Option<Vec<String>>,
    /// Changed paths that `buck2 dev` read during a request. They are handled next.
    queued: BTreeSet<String>,
}

/// A build that runs.
struct Building {
    generation: u64,
    /// The outputs stay stale until the server says with `ack` that it has them.
    stale_until_ack: bool,
    /// The outputs whose results the server was sent. If another command preempted the build,
    /// the daemon builds them again, but the server gets each once.
    sent: BTreeSet<String>,
    /// How many outputs the server got, and how many failed.
    built: usize,
    failed: usize,
}

/// What `buck2 dev` keeps of the outputs that it built, to say what changed.
#[derive(Default)]
struct Delivered {
    /// The digest of each output when it was last built, with its `argv` if it is a command
    /// line.
    digests: BuckMutMap<String, (Option<String>, Option<Vec<String>>)>,
    /// The digest of each sidecar, by the names of its output and its own, when it was last
    /// built.
    sidecar_digests: BuckMutMap<(String, String), Option<String>>,
    /// For each output that was built since `buck2 dev` last asked the daemon about changes:
    /// the paths that had changed by then, and that it has not asked about yet. The build
    /// used those changes, so they do not make the output stale.
    fresh: BTreeMap<String, BTreeSet<String>>,
}

impl Delivered {
    /// What the server gets of each of `outputs`, which were built after the changes in
    /// `known`.
    fn entries(
        &mut self,
        outputs: BTreeMap<String, DevBuiltOutput>,
        known: &BTreeSet<String>,
    ) -> serde_json::Map<String, serde_json::Value> {
        let mut entries = serde_json::Map::new();
        for (name, output) in outputs {
            self.fresh.insert(name.clone(), known.clone());
            let key = (output.digest.clone(), output.argv.clone());
            let changed = self.digests.get(&name) != Some(&key) || output.digest.is_none();
            self.digests.insert(name.clone(), key);
            let mut built = json!({
                "digest": output.digest,
                "changed": changed,
            });
            // An artifact has a `path`, and a command line has an `argv`.
            if let Some(path) = output.path {
                built["path"] = json!(path);
            }
            if let Some(argv) = output.argv {
                built["argv"] = json!(argv);
            }
            if !output.sidecars.is_empty() {
                // Each sidecar says by itself whether it changed.
                let mut sidecars = serde_json::Map::new();
                for (sidecar, artifact) in output.sidecars {
                    let key = (name.clone(), sidecar.clone());
                    let changed = self.sidecar_digests.get(&key) != Some(&artifact.digest)
                        || artifact.digest.is_none();
                    self.sidecar_digests.insert(key, artifact.digest.clone());
                    sidecars.insert(
                        sidecar,
                        json!({
                            "path": artifact.path,
                            "digest": artifact.digest,
                            "changed": changed,
                        }),
                    );
                }
                built["sidecars"] = serde_json::Value::Object(sidecars);
            }
            entries.insert(name, built);
        }
        entries
    }
}

/// Send the server a `built` with results of the build that runs. `known` has the changes that
/// `buck2 dev` has heard of: the daemon stops a build when a file that it builds from changes,
/// so the build used them. `last` says that no more results of the build follow. Returns
/// whether the server got it.
fn send_built(
    link: &mut ServerLink,
    tracked: &mut BTreeMap<String, Tracked>,
    delivered: &mut Delivered,
    building: &mut Building,
    built: DevBuilt,
    known: &BTreeSet<String>,
    last: bool,
) -> bool {
    let DevBuilt {
        mut outputs,
        mut failed,
    } = built;
    outputs.retain(|name, _| !building.sent.contains(name));
    failed.retain(|name, _| !building.sent.contains(name));
    if outputs.is_empty() && failed.is_empty() && !last {
        return false;
    }
    building.sent.extend(outputs.keys().cloned());
    building.sent.extend(failed.keys().cloned());
    building.built += outputs.len();
    building.failed += failed.len();
    let names: Vec<String> = outputs.keys().cloned().collect();
    let mut message = json!({
        "type": "built",
        "gen": building.generation,
        "outputs": delivered.entries(outputs, known),
        "last_of_generation": last,
    });
    // The outputs that failed keep the digests of the last time they built, and stay stale.
    if !failed.is_empty() {
        message["failed"] = json!(failed);
    }
    let got = link.send_result(building.generation, message, last);
    if got && !building.stale_until_ack {
        // The server has these outputs as the files are now.
        for name in names {
            if let Some(tracked) = tracked.get_mut(&name) {
                tracked.received(building.generation);
            }
        }
    }
    got
}

/// What became of analysing the target again.
#[derive(Default)]
struct Resolved {
    /// The errors, if it failed. The server has not been told.
    failed: Option<Vec<String>>,
    /// Whether it started the server again, or tried to.
    respawned: bool,
}

/// What became of a daemon request.
enum Outcome {
    Done(DevResponse),
    /// The daemon ran the request, but it failed. The console has already shown the errors.
    Failed(Vec<String>),
    /// The session is ending, so `buck2 dev` dropped the request.
    Interrupted,
}

#[cfg(unix)]
impl<'a> Session<'a> {
    /// Analyse the target and start the server. `Err` is the result of Ctrl-C before the server
    /// started.
    async fn start(
        mut daemon: Daemon<'a>,
        partials: mpsc::UnboundedReceiver<DevBuilt>,
        server_args: Vec<String>,
        debounce: Duration,
        listener: Listener,
        server_rx: mpsc::UnboundedReceiver<ServerEvent>,
        mut signals: Signals,
        console: Console,
        changes: FileChanges,
    ) -> buck2_error::Result<Result<Session<'a>, ExitResult>> {
        let response = {
            let request = daemon.request(|| DevAction::Server);
            tokio::pin!(request);
            loop {
                tokio::select! {
                    response = &mut request => break response?,
                    signal = signals.recv() => match signal {
                        Signal::Quit => return Ok(Err(ExitResult::signal_interrupt())),
                        // The daemon request's event loop stops `buck2 dev`.
                        Signal::Suspend => console.send(SessionUpdate::Stop),
                        Signal::Continue | Signal::Child | Signal::Resize => {}
                    },
                }
            }
        };
        let server_info = match response {
            Ok(DevResponse::Server(info)) => info,
            Ok(_) => return Err(unexpected_response()),
            Err(Failed(messages)) => return Err(could_not_start(messages)),
        };
        let mut session = Session {
            daemon,
            server_args,
            debounce,
            changes,
            missed: false,
            missed_queued: false,
            watched: Watched::default(),
            server_info,
            listener,
            server_rx,
            link: ServerLink {
                server: None,
                requests: VecDeque::new(),
                exit: None,
                console,
            },
            signals,
            status: String::new(),
            last_build: None,
            generation: 0,
            partials,
            building: None,
            delivered: Delivered::default(),
            tracked: BTreeMap::new(),
            stopped: BTreeMap::new(),
            unacted: BTreeSet::new(),
            resolve_due: false,
            graph_dirty: false,
            restart_due: false,
            watch_failed: false,
            watch_failure: None,
            resolve_failure: None,
            queued: BTreeSet::new(),
        };
        if session.resources_up()? {
            session.spawn().await?;
        } else {
            // The next change that starts the server sets up the resources again.
            session.restart_due = true;
        }
        Ok(Ok(session))
    }

    async fn run(&mut self) -> buck2_error::Result<ExitResult> {
        // The server starts while the daemon finds what the server and the outputs claim, which
        // may build what dynamic actions need. `buck2 dev` handles the server's requests
        // afterwards.
        let request = DevWatchRequest {
            paths: Vec::new(),
            previous: None,
            missed: false,
        };
        match self.request(|| DevAction::Watch(request.clone())).await? {
            Outcome::Done(DevResponse::Watch(watched)) => {
                self.take_watched(watched.state, watched.incomplete, watched.outputs.targets)?;
            }
            Outcome::Done(_) => return Err(unexpected_response()),
            Outcome::Failed(messages) => return Err(could_not_start(messages)),
            Outcome::Interrupted => {}
        }
        loop {
            if let Some(exit) = self.link.exit.take() {
                return Ok(exit);
            }
            self.show_status();
            if let Some(Request::Sync { id }) = self.link.requests.front() {
                let id = *id;
                if self.sync_changes().await? {
                    // Handled changes: answer once they have settled.
                    continue;
                }
                self.link.requests.pop_front();
                self.link.send(json!({"type": "_synced", "id": id}));
                continue;
            }
            if let Some(request) = self.link.requests.pop_front() {
                self.on_request(request).await?;
                continue;
            }
            if !self.queued.is_empty() || std::mem::take(&mut self.missed_queued) {
                // Files changed during the last request.
                let paths = std::mem::take(&mut self.queued);
                let paths = self.debounce(paths).await;
                self.on_changes(paths).await?;
                continue;
            }
            tokio::select! {
                change = self.changes.next() => {
                    let mut paths = BTreeSet::new();
                    self.missed |= collect_paths(change, &mut paths, &mut self.link.exit);
                    let paths = self.debounce(paths).await;
                    self.on_changes(paths).await?;
                }
                Some(event) = self.server_rx.recv() => {
                    let stale = stale_message(&self.tracked, &self.server_info.project_root);
                    self.link.on_event(event, &self.server_info, stale)?;
                }
                status = server_exited(&mut self.link.server) => {
                    self.link.on_exit(status, &mut self.signals).await?;
                }
                signal = self.signals.recv() => self.link.on_signal(signal)?,
                // While a daemon request runs, its event loop does this.
                update = self.daemon.events_ctx.next_session_update() => {
                    self.daemon.events_ctx.handle_session_update(update).await?;
                }
            }
        }
    }

    /// Between commands the console shows one line: what `buck2 dev` watches, whether the server
    /// runs, and how the last build went.
    fn show_status(&mut self) {
        let server = match &self.link.server {
            Some(_) => "the server is running",
            None => "the server starts again after the next change",
        };
        let mut status = format!(
            "buck2 dev: watching {} targets, {server}",
            self.watched.targets
        );
        if let Some((result, took)) = &self.last_build {
            status.push_str(&format!(
                ", last build {result} in {:.1}s",
                took.as_secs_f64()
            ));
        }
        if status != self.status {
            self.status = status.clone();
            self.link.console.send(SessionUpdate::Status(status));
        }
    }

    /// Stop the server before `buck2 dev` exits, and then have the daemon release the local
    /// resources. Another Ctrl-C kills the server at once.
    async fn shutdown(&mut self) {
        let mut interrupted = false;
        if let Some(mut server) = self.link.server.take() {
            interrupted = server.stop(&mut self.signals).await;
        }
        // The daemon holds the local resources. Wait until it has released them, so that what
        // they used, such as a port, is free once `buck2 dev` exits. Another Ctrl-C stops
        // waiting; the daemon releases them anyway.
        if !interrupted && !self.server_info.resources.is_empty() {
            let release = self.daemon.request(|| DevAction::Release);
            tokio::select! {
                _ = release => {}
                () = self.signals.quit() => {}
            }
        }
        // Show what the server wrote as it stopped, and take the status line off the terminal.
        self.link.console.send(SessionUpdate::Status(String::new()));
        while let Ok(update) = tokio::time::timeout(
            Duration::from_millis(100),
            self.daemon.events_ctx.next_session_update(),
        )
        .await
        {
            if self
                .daemon
                .events_ctx
                .handle_session_update(update)
                .await
                .is_err()
            {
                break;
            }
        }
    }

    /// Before `buck2 dev` answers `_sync`: handle the changes it has queued and those that reach it
    /// within a moment. Returns whether there were any.
    async fn sync_changes(&mut self) -> buck2_error::Result<bool> {
        let mut paths = std::mem::take(&mut self.queued);
        let mut missed = std::mem::take(&mut self.missed_queued);
        while let Ok(change) =
            tokio::time::timeout(Duration::from_millis(20), self.changes.next()).await
        {
            missed |= collect_paths(change, &mut paths, &mut self.link.exit);
        }
        if self.link.exit.is_some() || (paths.is_empty() && !missed) {
            return Ok(false);
        }
        self.missed |= missed;
        let paths = self.debounce(paths).await;
        self.on_changes(paths).await?;
        Ok(true)
    }

    /// Add the paths of further changes to `paths` until files stop changing, but for no longer
    /// than `MAX_DEBOUNCE`, in case they never do.
    async fn debounce(&mut self, mut paths: BTreeSet<String>) -> BTreeSet<String> {
        let deadline = tokio::time::Instant::now() + MAX_DEBOUNCE;
        let mut quiet_until = tokio::time::Instant::now() + self.debounce;
        loop {
            let wait_until = quiet_until.min(deadline);
            match tokio::time::timeout_at(wait_until, self.changes.next()).await {
                Ok(change) => {
                    self.missed |= collect_paths(change, &mut paths, &mut self.link.exit);
                    quiet_until = tokio::time::Instant::now() + self.debounce;
                }
                Err(_) => return paths,
            }
        }
    }

    async fn on_request(&mut self, request: Request) -> buck2_error::Result<()> {
        match request {
            Request::Build {
                outputs,
                streaming,
                stale_until_ack,
            } => {
                // From now on, the server hears when these are stale.
                for name in &outputs {
                    self.tracked.entry(name.clone()).or_default();
                }
                // The graph changed, or the analysis or the server's build failed, since the
                // server was last built. This build uses the files as they are, so the server has
                // to be as new as they are.
                if self.resolve_due {
                    let resolved = self.reresolve(true).await?;
                    if let Some(messages) = resolved.failed {
                        // The build goes ahead: if the analysis failed, so does the build, with
                        // the same errors. If only the server's build failed, the outputs may
                        // still build.
                        self.resolve_failure = Some(messages);
                    } else if resolved.respawned {
                        // The server that asked is gone; the new one asks for what it needs.
                        return Ok(());
                    }
                }
                self.build(outputs, streaming, stale_until_ack).await?;
            }
            Request::Ack {
                r#gen: generation,
                outputs,
            } => self.ack(generation, outputs)?,
            Request::Restart => self.restart().await?,
            // Answered as soon as it arrives.
            Request::Hello { .. } => {}
            // Answered by `run`.
            Request::Sync { .. } => {}
        }
        Ok(())
    }

    /// Ask the daemon what changed, and act on it. The server's command is built again if it may
    /// be new, and the server hears which of the outputs that it asked for are stale. No output
    /// is built.
    async fn on_changes(&mut self, paths: BTreeSet<String>) -> buck2_error::Result<()> {
        self.unacted.extend(paths);
        let missed = self.missed;
        if self.unacted.is_empty() && !missed && self.stopped.is_empty() {
            return Ok(());
        }
        let request = DevWatchRequest {
            paths: self.unacted.iter().cloned().collect(),
            previous: self.watched.state.clone(),
            missed,
        };
        let watched = match self.request(|| DevAction::Watch(request.clone())).await? {
            Outcome::Done(DevResponse::Watch(watched)) => watched,
            Outcome::Done(_) => return Err(unexpected_response()),
            Outcome::Failed(messages) => {
                // The changes stay in `unacted`, and the next change asks again. Once the
                // analysis works again, the server gets `graph_changed`.
                self.resolve_due = true;
                self.graph_dirty = true;
                self.watch_failed = true;
                // The same errors after another change are no news: the change did not touch
                // what failed. Sending them again would also make a loop of a server that writes
                // a file in the project for each message.
                if self.watch_failure.as_ref() != Some(&messages) {
                    self.watch_failure = Some(messages.clone());
                    self.send_failed(messages);
                }
                return Ok(());
            }
            Outcome::Interrupted => return Ok(()),
        };
        let recovered = std::mem::take(&mut self.watch_failed);
        self.watch_failure = None;
        let DevWatched {
            state,
            graph_changed,
            incomplete,
            outputs: changes,
        } = watched;
        // Without a state there is nothing to compare with: the session started without an
        // answer.
        let graph_changed = self.watched.state.is_none() || graph_changed;
        let relevant = recovered || graph_changed || changes.server || !changes.outputs.is_empty();
        let fresh = std::mem::take(&mut self.delivered.fresh);
        let stopped = std::mem::take(&mut self.stopped);

        self.missed = false;
        self.unacted.clear();
        self.take_watched(state, incomplete, changes.targets)?;
        self.graph_dirty |= graph_changed;
        self.resolve_due |= graph_changed;

        // The server's command is built again only if it may be new, and a server that exited
        // starts again after a change that is something to it or to an output.
        if changes.server
            || recovered
            || self.resolve_due
            || (self.link.server.is_none() && relevant)
        {
            let resolved = self.reresolve(relevant).await?;
            if let Some(messages) = &resolved.failed {
                if relevant || self.resolve_failure.as_ref() != Some(messages) {
                    self.send_failed(messages.clone());
                }
                self.resolve_failure = Some(messages.clone());
                if self.link.server.is_none() {
                    // The next analysis starts the server, which asks for what it needs.
                    self.restart_due = true;
                    return Ok(());
                }
            }
            if resolved.respawned {
                // The new server has asked for nothing, so nothing is stale for it.
                return Ok(());
            }
        }

        // What the changes are to the outputs that the server asked for.
        let mut affected = BTreeSet::new();
        for (name, list) in changes.outputs {
            let Some(tracked) = self.tracked.get_mut(&name) else {
                continue;
            };
            // A build since the changes were read has used them already. Not a change to the
            // graph, though: a new file that a target takes in does not stop a build that is
            // running.
            let fresh = fresh.get(&name).filter(|_| !graph_changed);
            for change in list {
                let used = match (fresh, &change.path) {
                    (Some(fresh), Some(path)) => fresh.contains(path),
                    _ => false,
                };
                if !used {
                    tracked.add(change, self.generation);
                    affected.insert(name.clone());
                }
            }
        }
        // A build that the daemon stopped is stale, whatever the answer says of its outputs.
        for (name, paths) in stopped {
            let Some(tracked) = self.tracked.get_mut(&name) else {
                continue;
            };
            if affected.insert(name) {
                for path in paths {
                    tracked.add(
                        DevChange {
                            path: Some(path),
                            scopes: Vec::new(),
                        },
                        self.generation,
                    );
                }
            }
        }
        if !affected.is_empty() {
            if let Some(stale) = stale_message(&self.tracked, &self.server_info.project_root) {
                self.link.send(stale);
            }
        }
        Ok(())
    }

    /// Keep what the daemon said about the targets of the server and the outputs.
    fn take_watched(
        &mut self,
        state: DevWatchState,
        incomplete: Vec<String>,
        targets: usize,
    ) -> buck2_error::Result<()> {
        self.watched.targets = targets;
        if incomplete != self.watched.incomplete {
            for message in &incomplete {
                self.link.console.message(format!(
                    "`buck2 dev` could not find every file to watch, so it watches more files: {}",
                    message
                ))?;
            }
        }
        self.watched.incomplete = incomplete;
        self.watched.state = Some(state);
        Ok(())
    }

    /// Send a request to the daemon, and handle the server while it runs. File changes during
    /// the request do not stop it: `buck2 dev` reads them, and handles them once it is done. Only
    /// the end of the session drops the request, which cancels it in the daemon.
    async fn request(&mut self, action: impl Fn() -> DevAction) -> buck2_error::Result<Outcome> {
        if self.link.exit.is_some() {
            return Ok(Outcome::Interrupted);
        }
        let Session {
            daemon,
            changes,
            missed,
            missed_queued,
            queued,
            unacted,
            server_info,
            server_rx,
            link,
            tracked,
            signals,
            partials,
            building,
            delivered,
            ..
        } = &mut *self;
        // Results of a streaming build go to the server as they arrive.
        let mut deliver = |built: DevBuilt,
                           queued: &BTreeSet<String>,
                           link: &mut ServerLink,
                           tracked: &mut BTreeMap<String, Tracked>| {
            if let Some(building) = building.as_mut() {
                let known: BTreeSet<String> = queued.union(unacted).cloned().collect();
                send_built(link, tracked, delivered, building, built, &known, false);
            }
        };
        let request = daemon.request(action);
        tokio::pin!(request);
        loop {
            tokio::select! {
                result = &mut request => {
                    let result = result?;
                    // The results that the build sent before its response.
                    while let Ok(built) = partials.try_recv() {
                        deliver(built, queued, link, tracked);
                    }
                    return Ok(match result {
                        Ok(response) => Outcome::Done(response),
                        Err(Failed(messages)) => Outcome::Failed(messages),
                    });
                }
                Some(built) = partials.recv() => deliver(built, queued, link, tracked),
                // The server is not kept waiting while the daemon works. Its requests are handled
                // after this one.
                Some(event) = server_rx.recv() => {
                    let stale = stale_message(tracked, &server_info.project_root);
                    link.on_event(event, server_info, stale)?;
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
                change = changes.next() => {
                    if collect_paths(change, queued, &mut link.exit) {
                        *missed = true;
                        *missed_queued = true;
                    }
                    if link.exit.is_some() {
                        return Ok(Outcome::Interrupted);
                    }
                }
            }
        }
    }

    /// Build `outputs`, which the server asked for, and tell the server. An output that it
    /// gets is no longer stale, or, with `stale_until_ack`, once the server says that it has
    /// it. With `streaming`, the server gets each output as soon as it is built. The daemon
    /// stops the build if a file that an output that is not built yet builds from changes;
    /// other changes during the build are handled after it.
    async fn build(
        &mut self,
        outputs: Vec<String>,
        streaming: bool,
        stale_until_ack: bool,
    ) -> buck2_error::Result<()> {
        let outputs: Vec<String> = outputs
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        // The changes since the server last got each output.
        let root = &self.server_info.project_root;
        let files: BTreeSet<String> = outputs
            .iter()
            .filter_map(|name| self.tracked.get(name))
            .flat_map(|tracked| tracked.files().map(|path| absolute(root, path)))
            .collect();
        self.generation += 1;
        let generation = self.generation;
        self.link.send_building(
            generation,
            json!({
                "type": "building",
                "gen": generation,
                "outputs": outputs,
                "files": files,
            }),
        );

        let started = Instant::now();
        self.building = Some(Building {
            generation,
            stale_until_ack,
            sent: BTreeSet::new(),
            built: 0,
            failed: 0,
        });
        let outcome = self
            .request(|| DevAction::Build {
                outputs: outputs.clone(),
                streaming,
            })
            .await;
        let mut building = self
            .building
            .take()
            .ok_or_else(|| internal_error!("The build that ran is gone"))?;
        match outcome? {
            Outcome::Done(DevResponse::Build(built))
                if !streaming && built.outputs.is_empty() && !built.failed.is_empty() =>
            {
                // Nothing built.
                let error: Vec<String> = built.failed.into_values().collect();
                self.link.send_result(
                    generation,
                    json!({
                        "type": "failed",
                        "gen": generation,
                        "error": error.join("\n"),
                    }),
                    true,
                );
                self.last_build = Some(("failed", started.elapsed()));
            }
            Outcome::Done(DevResponse::Build(built)) => {
                let known: BTreeSet<String> = self.queued.union(&self.unacted).cloned().collect();
                send_built(
                    &mut self.link,
                    &mut self.tracked,
                    &mut self.delivered,
                    &mut building,
                    built,
                    &known,
                    true,
                );
                let result = match (building.built, building.failed) {
                    (_, 0) => "succeeded",
                    (0, _) => "failed",
                    _ => "partly failed",
                };
                self.last_build = Some((result, started.elapsed()));
            }
            Outcome::Done(DevResponse::Interrupted(interrupted)) => {
                self.link.send_result(
                    generation,
                    json!({"type": "cancelled", "gen": generation}),
                    true,
                );
                // The outputs that the server did not get are stale once `buck2 dev` has asked
                // about the changes. It has them from the file watcher too, but asks about them
                // in any case.
                let paths: Vec<String> = if interrupted.missed {
                    vec![String::new()]
                } else {
                    interrupted.paths.clone()
                };
                for name in outputs.iter().filter(|name| !building.sent.contains(*name)) {
                    self.stopped
                        .entry(name.clone())
                        .or_default()
                        .extend(paths.iter().cloned());
                }
                self.queued.extend(interrupted.paths);
                self.missed |= interrupted.missed;
                self.missed_queued |= interrupted.missed;
            }
            Outcome::Done(_) => return Err(unexpected_response()),
            Outcome::Failed(messages) => {
                self.link.send_result(
                    generation,
                    json!({
                        "type": "failed",
                        "gen": generation,
                        "error": messages.join("\n"),
                    }),
                    true,
                );
                self.last_build = Some(("failed", started.elapsed()));
            }
            // The session is ending: the server is stopping too.
            Outcome::Interrupted => {}
        }
        Ok(())
    }

    /// The server is up to date with `outputs` as build `generation` left them, built by it or
    /// not: changes heard of before that build started no longer make them stale.
    fn ack(&mut self, generation: u64, outputs: Vec<String>) -> buck2_error::Result<()> {
        if generation > self.generation {
            return self.link.console.message(format!(
                "`buck2 dev` ignored an `ack` from the server for build {generation}, which has not started"
            ));
        }
        for name in outputs {
            if !self.server_info.outputs.contains(&name) {
                self.link.console.message(format!(
                    "`buck2 dev` ignored an `ack` from the server for `{name}`, which is not an output"
                ))?;
                continue;
            }
            self.tracked.entry(name).or_default().received(generation);
        }
        Ok(())
    }

    /// Analyse the target again: build the server, and start it again if a restart is due, or
    /// if its command line or the contents of what that names changed. A server that exited
    /// starts again only with `start_exited`. If the analysis or the build fails, `buck2 dev`
    /// analyses again after the next change, and the caller tells the server.
    async fn reresolve(&mut self, start_exited: bool) -> buck2_error::Result<Resolved> {
        self.resolve_due = true;
        let info = match self.request(|| DevAction::Server).await? {
            Outcome::Done(DevResponse::Server(info)) => info,
            Outcome::Done(_) => return Err(unexpected_response()),
            Outcome::Failed(messages) => {
                // Once the analysis works again, the server gets `graph_changed`.
                self.graph_dirty = true;
                return Ok(Resolved {
                    failed: Some(messages),
                    respawned: false,
                });
            }
            Outcome::Interrupted => return Ok(Resolved::default()),
        };
        self.resolve_due = false;
        self.resolve_failure = None;
        // Start the server again if a restart is due, or it would start differently, or it
        // would run something else: what its command line names has other contents, or a local
        // resource was set up anew, or the resources give it other variables.
        let restart = std::mem::take(&mut self.restart_due)
            || match &self.link.server {
                Some(server) => {
                    server.argv != info.server
                        || server.interactive != info.interactive
                        || server.digest != info.digest
                        || server.env != info.env
                        || info
                            .resources
                            .iter()
                            .any(|resource| !server.resources.contains(resource))
                }
                None => start_exited,
            };
        self.server_info = info;
        if !self.resources_up()? {
            // The server runs only with all its resources. The next analysis sets them up
            // again, and starts it.
            if let Some(mut server) = self.link.server.take() {
                if server.stop(&mut self.signals).await {
                    self.link.exit = Some(ExitResult::signal_interrupt());
                }
            }
            self.restart_due = true;
            return Ok(Resolved {
                failed: None,
                respawned: true,
            });
        }
        if restart {
            self.respawn().await?;
        }
        if std::mem::take(&mut self.graph_dirty) {
            self.link.send(json!({"type": "graph_changed"}));
        }
        Ok(Resolved {
            failed: None,
            respawned: restart,
        })
    }

    /// Say what the last analysis did to the local resources. Returns whether all of them run.
    fn resources_up(&self) -> buck2_error::Result<bool> {
        for name in &self.server_info.resources_exited {
            self.link.console.message(format!(
                "Local resource `{name}` had exited. `buck2 dev` set it up again."
            ))?;
        }
        for error in &self.server_info.resource_errors {
            self.link.console.message(format!(
                "{error}\n`buck2 dev` tries again, and starts the server, after the next change."
            ))?;
        }
        Ok(self.server_info.resource_errors.is_empty())
    }

    fn send_failed(&mut self, messages: Vec<String>) {
        self.generation += 1;
        self.link.send(json!({
            "type": "failed",
            "gen": self.generation,
            "error": messages.join("\n"),
        }));
    }

    /// Rebuild the server and start it again. If that fails, the next analysis starts it again.
    async fn restart(&mut self) -> buck2_error::Result<()> {
        self.restart_due = true;
        if let Some(messages) = self.reresolve(true).await?.failed {
            self.resolve_failure = Some(messages.clone());
            self.send_failed(messages);
        }
        Ok(())
    }

    /// Stop the server, if it is running, and start it again. If it does not start, the next
    /// change to what the server or an output is built from tries again.
    async fn respawn(&mut self) -> buck2_error::Result<()> {
        // Stop the old server first, so that the new one can use its resources, such as a port.
        if let Some(mut server) = self.link.server.take() {
            if server.stop(&mut self.signals).await {
                self.link.exit = Some(ExitResult::signal_interrupt());
                return Ok(());
            }
        }
        // Files that changed while the old server stopped, or while its replacement was built:
        // build the replacement from them, so that they need not restart it again.
        let mut changed = std::mem::take(&mut self.queued);
        while let Ok(change) =
            tokio::time::timeout(Duration::from_millis(20), self.changes.next()).await
        {
            if collect_paths(change, &mut changed, &mut self.link.exit) {
                self.missed = true;
                self.missed_queued = true;
            }
        }
        if self.link.exit.is_some() {
            return Ok(());
        }
        if !changed.is_empty() {
            match self.request(|| DevAction::Server).await? {
                Outcome::Done(DevResponse::Server(info)) => {
                    self.server_info = info;
                    if !self.resources_up()? {
                        self.restart_due = true;
                        self.queued.extend(changed);
                        return Ok(());
                    }
                }
                Outcome::Done(_) => return Err(unexpected_response()),
                Outcome::Failed(messages) => {
                    // No server runs; the next analysis starts it.
                    self.graph_dirty = true;
                    self.resolve_due = true;
                    self.restart_due = true;
                    self.queued.extend(changed);
                    self.send_failed(messages);
                    return Ok(());
                }
                Outcome::Interrupted => return Ok(()),
            }
            self.queued.extend(changed);
        }
        // The new server asks for what it needs.
        self.link.requests.clear();
        self.delivered = Delivered::default();
        // The new server has asked for nothing yet.
        self.tracked.clear();
        self.stopped.clear();
        if let Err(e) = self.spawn().await {
            self.link.console.message(format!(
                "{:#}\n`buck2 dev` starts the server again after the next change.",
                e
            ))?;
        }
        Ok(())
    }

    /// Start the server. It gets `ready` once it connects and sends `hello`.
    async fn spawn(&mut self) -> buck2_error::Result<()> {
        let interactive = self.server_info.interactive;
        if interactive {
            // An interactive server has the terminal, and draws on it as it likes. The
            // superconsole, which redraws the lines at the bottom of the terminal, would draw
            // over it, so from now on `buck2 dev` uses the simple console.
            self.daemon.events_ctx.erase_interactive_output().await?;
            self.link.console.drop_canvas();
        }
        // With the superconsole, the server's output goes above its canvas. Otherwise the server
        // writes to the terminal itself.
        let output = (!interactive && self.link.console.has_canvas())
            .then(|| self.link.console.updates.clone());
        let first_conn = self.listener.next_conn();
        let argv: Vec<String> = self
            .server_info
            .server
            .iter()
            .chain(&self.server_args)
            .cloned()
            .collect();
        let process = ServerProcess::spawn(
            &argv,
            &self.server_info.project_root,
            &self.server_info.env,
            interactive,
            self.listener.address(),
            output,
        )?;
        self.link.server = Some(Server {
            process,
            argv: self.server_info.server.clone(),
            interactive: self.server_info.interactive,
            digest: self.server_info.digest.clone(),
            env: self.server_info.env.clone(),
            resources: self.server_info.resources.clone(),
            unintroduced: BuckMutMap::default(),
            conn: None,
            first_conn,
            announced: None,
            console: self.link.console.clone(),
        });
        Ok(())
    }
}

/// Add the path of `change` to `paths`. Returns whether changes were missed instead. If no
/// more changes will come, because the daemon went away, set `exit`: the session ends.
#[cfg(unix)]
fn collect_paths(
    change: FileChange,
    paths: &mut BTreeSet<String>,
    exit: &mut Option<ExitResult>,
) -> bool {
    match change {
        FileChange::Path(path) => {
            paths.insert(path);
            false
        }
        FileChange::Missed => true,
        FileChange::Stopped(e) => {
            exit.get_or_insert_with(|| ExitResult::err(daemon_gone(e)));
            false
        }
    }
}
