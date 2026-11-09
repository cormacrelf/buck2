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

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
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
use buck2_error::BuckErrorContext;
use buck2_error::BuckErrorOptionContext;
use buck2_error::ErrorTag;
use buck2_error::buck2_error;
use buck2_hash::BuckMutMap;
use buck2_hash::BuckMutSet;
use notify::EventKind;
use notify::RecursiveMode;
use notify::Watcher;
use serde_json::json;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::io::Lines;
use tokio::process::Child;
use tokio::process::ChildStdin;
use tokio::process::ChildStdout;
use tokio::sync::mpsc;

/// Run a target's `DevInfo` server, and rebuild as source files change.
///
/// The target must return a `DevInfo` provider. `buck2 dev` builds and starts its server, then
/// watches the source files of the target's dependency graph. When a file changes, the first
/// `DevWatch` whose files include it decides what happens: build some of the `DevInfo` outputs
/// and tell the server, rebuild and restart the server, or only tell the server.
///
/// The server reads JSON lines from its stdin. It may write JSON requests to its stdout, such as
/// `{"type": "build", "outputs": ["name"]}`; other lines are printed. See the `buck2 dev` page of
/// the rule author documentation for the messages.
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
        let daemon = Daemon {
            buckd,
            events_ctx,
            connect_options: connect_options(&self, ctx)?,
            paths: ctx.paths()?.clone(),
            context,
            target: self.target.clone(),
            target_cfg: self.target_cfg.target_cfg(),
        };
        let session = Session::start(daemon, Duration::from_millis(self.debounce_ms)).await?;
        match session.run().await {
            Ok(result) => result,
            Err(e) => ExitResult::err(e),
        }
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

/// The running server process.
struct Server {
    argv: Vec<String>,
    child: Child,
    /// `None` once `buck2 dev` has closed it to stop the server.
    stdin: Option<ChildStdin>,
    stdout: Lines<BufReader<ChildStdout>>,
}

impl Server {
    fn spawn(info: &DevServer) -> buck2_error::Result<Server> {
        let (program, args) = info.server.split_first().ok_or_else(|| {
            buck2_error!(
                ErrorTag::Input,
                "The `DevInfo` server command line is empty"
            )
        })?;
        let mut child = tokio::process::Command::new(program)
            .args(args)
            .current_dir(&info.project_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_buck_error_context(|| {
                format!("Failed to start the `buck2 dev` server `{program}`")
            })?;
        let stdin = child.stdin.take().internal_error("stdin was piped")?;
        let stdout = child.stdout.take().internal_error("stdout was piped")?;
        Ok(Server {
            argv: info.server.clone(),
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout).lines(),
        })
    }

    async fn send(&mut self, message: serde_json::Value) -> buck2_error::Result<()> {
        let mut line = serde_json::to_vec(&message)?;
        line.push(b'\n');
        // A server that stopped reading is noticed when it exits.
        if let Some(stdin) = &mut self.stdin {
            if stdin.write_all(&line).await.is_ok() {
                let _ignored = stdin.flush().await;
            }
        }
        Ok(())
    }

    /// Close the server's stdin, so that it can stop what it started, and kill it if it has not
    /// exited after `SERVER_STOP_TIMEOUT`.
    async fn stop(&mut self) {
        self.stdin = None;
        if tokio::time::timeout(SERVER_STOP_TIMEOUT, self.child.wait())
            .await
            .is_err()
        {
            let _ignored = self.child.kill().await;
        }
    }
}

impl Drop for Server {
    /// `buck2 dev` is exiting, perhaps on Ctrl-C. Close the server's stdin and ask it to stop;
    /// there is no time to wait for it.
    fn drop(&mut self) {
        self.stdin = None;
        if matches!(self.child.try_wait(), Ok(None)) {
            #[cfg(unix)]
            if let Some(pid) = self.child.id() {
                // SAFETY: sending a signal to a child process we have not yet waited for.
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGTERM);
                }
                return;
            }
            let _ignored = self.child.start_kill();
        }
    }
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

/// How long `buck2 dev` waits for a server to exit after closing its stdin.
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
    std::fs::read(path).ok().map(|bytes| blake3::hash(&bytes))
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

struct Session<'a> {
    daemon: Daemon<'a>,
    debounce: Duration,
    events_tx: mpsc::UnboundedSender<notify::Result<notify::Event>>,
    events_rx: mpsc::UnboundedReceiver<notify::Result<notify::Event>>,
    watched: Watched,
    /// The server's command line and the names of the outputs, from the last analysis.
    server_info: DevServer,
    server: Server,
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

impl<'a> Session<'a> {
    async fn start(mut daemon: Daemon<'a>, debounce: Duration) -> buck2_error::Result<Session<'a>> {
        let could_not_start = |messages: Vec<String>| {
            buck2_error!(
                ErrorTag::Input,
                "`buck2 dev` could not start: {}",
                messages.join("\n")
            )
        };
        let server_info = match daemon.request(|| DevAction::Server).await? {
            Ok(DevResponse::Server(info)) => info,
            Ok(_) => return Err(unexpected_response()),
            Err(Failed(messages)) => return Err(could_not_start(messages)),
        };
        let mut server = Server::spawn(&server_info)?;
        server
            .send(json!({"type": "ready", "outputs": server_info.outputs}))
            .await?;
        // The server starts while `buck2 dev` finds the files to watch, which may build what
        // dynamic actions need. It handles the server's requests afterwards.
        let watched = match daemon.request(|| DevAction::Watch).await? {
            Ok(DevResponse::Watch(watched)) => watched,
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
            server,
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
        session.install_watch(watched)?;
        Ok(session)
    }

    async fn run(mut self) -> buck2_error::Result<ExitResult> {
        loop {
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
                line = self.server.stdout.next_line() => {
                    match line {
                        Ok(Some(line)) => self.on_server_line(line).await?,
                        // The server closed its stdout; wait for it to exit.
                        Ok(None) | Err(_) => {
                            let status = self.server.child.wait().await?;
                            return server_exit(status);
                        }
                    }
                }
                status = self.server.child.wait() => {
                    return server_exit(status?);
                }
            }
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

    async fn on_server_line(&mut self, line: String) -> buck2_error::Result<()> {
        #[derive(serde::Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum Request {
            Build { outputs: Vec<String> },
            Restart,
        }
        match serde_json::from_str::<Request>(&line) {
            Ok(Request::Build { outputs }) => {
                if self.resolve_due {
                    self.reresolve().await?;
                }
                self.build(outputs, Vec::new()).await?;
                self.reset_pending().await?;
            }
            Ok(Request::Restart) => {
                self.restart().await?;
                self.reset_pending().await?;
            }
            Err(e) => {
                // A JSON object with a `type` is meant as a request: say what is wrong with it,
                // rather than print it as output.
                let is_request = serde_json::from_str::<serde_json::Value>(&line)
                    .ok()
                    .is_some_and(|v| v.get("type").is_some_and(|t| t.is_string()));
                if is_request {
                    buck2_client_ctx::eprintln!(
                        "`buck2 dev` ignored a request from the server: {}: {}",
                        e,
                        line
                    )?;
                } else {
                    buck2_client_ctx::println!("{}", line)?;
                }
            }
        }
        Ok(())
    }

    /// The server asked for a build or restart, so it now uses the files as they are: they are
    /// the new baseline.
    async fn reset_pending(&mut self) -> buck2_error::Result<()> {
        if let Ok(mut baseline) = self.baseline.lock() {
            for path in &self.pending {
                baseline.insert(path.clone(), Baseline::Hash(hash_file(path)));
            }
        }
        self.held.clear();
        self.held_graph_paths.clear();
        if !self.pending.is_empty() {
            self.pending.clear();
            self.server
                .send(json!({"type": "stale", "files": Vec::<String>::new()}))
                .await?;
        }
        Ok(())
    }

    async fn on_changes(&mut self, paths: BTreeSet<PathBuf>) -> buck2_error::Result<()> {
        let mut changed_sets: BTreeMap<usize, Vec<PathBuf>> = BTreeMap::new();
        let mut waiting: Vec<(PathBuf, Option<blake3::Hash>)> = Vec::new();
        let mut bzl_changed: Vec<PathBuf> = Vec::new();
        let mut graph_changed = false;

        for path in paths {
            let owner = self.watched.owners.get(&path).copied();
            if owner.is_none() && !self.watched.in_package(&path) {
                continue;
            }
            let hash = hash_file(&path);
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
            self.server
                .send(json!({"type": "stale", "files": self.pending}))
                .await?;
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
        if !outputs.is_empty() || !self.cancelled_outputs.is_empty() {
            self.build(outputs.into_iter().collect(), files).await?;
        }
        if restart {
            self.restart().await?;
        }
        Ok(())
    }

    /// Send a request to the daemon, and read file events while it runs. If a file changes that
    /// makes the request out of date, cancel it: dropping the request cancels it in the daemon,
    /// and the next request, which sees different files, preempts it if it has not stopped yet.
    async fn request(&mut self, action: impl Fn() -> DevAction) -> buck2_error::Result<Outcome> {
        let Session {
            daemon,
            events_rx,
            watched,
            last_seen,
            queued,
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
                event = events_rx.recv() => {
                    let Some(event) = event else { continue };
                    let mut paths = BTreeSet::new();
                    collect_paths(watched, event, &mut paths);
                    let interrupt = paths
                        .iter()
                        .any(|path| interrupts(watched, last_seen, path));
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
        self.server
            .send(json!({
                "type": "building",
                "gen": generation,
                "outputs": outputs,
                "files": files,
            }))
            .await?;

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
                self.server
                    .send(json!({"type": "built", "gen": generation, "outputs": message}))
                    .await?;
            }
            Outcome::Done(_) => return Err(unexpected_response()),
            Outcome::Failed(messages) => {
                self.server
                    .send(json!({
                        "type": "failed",
                        "gen": generation,
                        "error": messages.join("\n"),
                    }))
                    .await?;
            }
            Outcome::Interrupted => {
                self.cancelled_outputs.extend(outputs);
                self.cancelled_files.extend(files);
                self.server
                    .send(json!({"type": "cancelled", "gen": generation}))
                    .await?;
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
                self.send_failed(messages).await?;
                return Ok(Resolved::Failed);
            }
            Outcome::Interrupted => return Ok(Resolved::Interrupted),
        };
        let restart = std::mem::take(&mut self.restart_due) || info.server != self.server.argv;
        self.server_info = info;
        if restart {
            self.respawn().await?;
        }
        let watched = match self.request(|| DevAction::Watch).await? {
            Outcome::Done(DevResponse::Watch(watched)) => watched,
            Outcome::Done(_) => return Err(unexpected_response()),
            Outcome::Failed(messages) => {
                self.send_failed(messages).await?;
                return Ok(Resolved::Failed);
            }
            Outcome::Interrupted => return Ok(Resolved::Interrupted),
        };
        let changed = self.watched.changed_sets(&watched);
        self.install_watch(watched)?;
        if std::mem::take(&mut self.graph_dirty) {
            self.server.send(json!({"type": "graph_changed"})).await?;
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

    async fn send_failed(&mut self, messages: Vec<String>) -> buck2_error::Result<()> {
        self.generation += 1;
        self.server
            .send(json!({
                "type": "failed",
                "gen": self.generation,
                "error": messages.join("\n"),
            }))
            .await
    }

    /// Rebuild the server and start it again. If that fails or is interrupted, the next analysis
    /// starts it again.
    async fn restart(&mut self) -> buck2_error::Result<()> {
        self.restart_due = true;
        self.reresolve().await?;
        Ok(())
    }

    async fn respawn(&mut self) -> buck2_error::Result<()> {
        // Stop the old server first, so that the new one can use its resources, such as a port.
        self.server.stop().await;
        self.server = Server::spawn(&self.server_info)?;
        self.digests.clear();
        // The new server asks for what it needs.
        self.cancelled_outputs.clear();
        self.cancelled_files.clear();
        self.server
            .send(json!({"type": "ready", "outputs": self.server_info.outputs}))
            .await?;
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
    for path in event.paths {
        if watched.owners.contains_key(&path) || watched.in_package(&path) {
            paths.insert(path);
        }
    }
}

/// Whether a change to `path` makes the running request out of date, so that `buck2 dev` should
/// cancel it and send it again: the file has new content, and a change to it does not wait for
/// the server.
fn interrupts(
    watched: &Watched,
    last_seen: &BuckMutMap<PathBuf, Option<blake3::Hash>>,
    path: &Path,
) -> bool {
    !watched.waits(path) && last_seen.get(path) != Some(&hash_file(path))
}

fn server_exit(status: std::process::ExitStatus) -> buck2_error::Result<ExitResult> {
    if status.success() {
        Ok(ExitResult::success())
    } else {
        Ok(ExitResult::bail(format!(
            "The `buck2 dev` server exited with {status}"
        )))
    }
}
