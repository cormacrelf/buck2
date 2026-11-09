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
use buck2_cli_proto::new_generic::DevBuilt;
use buck2_cli_proto::new_generic::DevRequest;
use buck2_cli_proto::new_generic::DevResolved;
use buck2_cli_proto::new_generic::DevResponse;
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
use buck2_client_ctx::events_ctx::EventsCtx;
use buck2_client_ctx::exit_result::ExitResult;
use buck2_client_ctx::streaming::StreamingCommand;
use buck2_error::BuckErrorContext;
use buck2_error::BuckErrorOptionContext;
use buck2_error::ErrorTag;
use buck2_error::buck2_error;
use buck2_hash::BuckMutMap;
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
/// `DevWatch` whose targets use the file decides what happens: build some of the `DevInfo`
/// outputs and tell the server, rebuild and restart the server, or only tell the server.
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
        loop {
            let outcome = self
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
                .await?;
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

    async fn resolve(&mut self) -> buck2_error::Result<Result<DevResolved, Failed>> {
        Ok(match self.request(|| DevAction::Resolve).await? {
            Ok(DevResponse::Resolve(r)) => Ok(r),
            Ok(DevResponse::Build(_)) => {
                return Err(buck2_error!(
                    ErrorTag::InvalidEvent,
                    "Unexpected build response to a resolve request"
                ));
            }
            Err(e) => Err(e),
        })
    }

    async fn build(&mut self, outputs: &[String]) -> buck2_error::Result<Result<DevBuilt, Failed>> {
        Ok(
            match self
                .request(|| DevAction::Build {
                    outputs: outputs.to_vec(),
                })
                .await?
            {
                Ok(DevResponse::Build(b)) => Ok(b),
                Ok(DevResponse::Resolve(_)) => {
                    return Err(buck2_error!(
                        ErrorTag::InvalidEvent,
                        "Unexpected resolve response to a build request"
                    ));
                }
                Err(e) => Err(e),
            },
        )
    }
}

/// The running server process.
struct Server {
    argv: Vec<String>,
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
}

impl Server {
    fn spawn(resolved: &DevResolved) -> buck2_error::Result<Server> {
        let (program, args) = resolved.server.split_first().ok_or_else(|| {
            buck2_error!(
                ErrorTag::Input,
                "The `DevInfo` server command line is empty"
            )
        })?;
        let mut child = tokio::process::Command::new(program)
            .args(args)
            .current_dir(&resolved.project_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .with_buck_error_context(|| {
                format!("Failed to start the `buck2 dev` server `{program}`")
            })?;
        let stdin = child.stdin.take().internal_error("stdin was piped")?;
        let stdout = child.stdout.take().internal_error("stdout was piped")?;
        Ok(Server {
            argv: resolved.server.clone(),
            child,
            stdin,
            stdout: BufReader::new(stdout).lines(),
        })
    }

    async fn send(&mut self, message: serde_json::Value) -> buck2_error::Result<()> {
        let mut line = serde_json::to_vec(&message)?;
        line.push(b'\n');
        // A server that stopped reading is noticed when it exits.
        if self.stdin.write_all(&line).await.is_ok() {
            let _ignored = self.stdin.flush().await;
        }
        Ok(())
    }

    async fn stop(&mut self) {
        let _ignored = self.child.kill().await;
    }
}

/// Where a watched path belongs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    /// A source file or build file of a watch set.
    File(usize),
    /// A `.bzl` file loaded by a package in the graph.
    Graph,
}

/// The file sets of the current resolve, and the file watcher for them.
struct Watched {
    resolved: DevResolved,
    owners: BuckMutMap<PathBuf, Owner>,
    /// Package directories, with their watch set. A new file in one may be a new source file.
    dirs: BuckMutMap<PathBuf, usize>,
    _watcher: notify::RecommendedWatcher,
}

impl Watched {
    fn new(
        resolved: DevResolved,
        events: mpsc::UnboundedSender<notify::Result<notify::Event>>,
    ) -> buck2_error::Result<Watched> {
        let mut owners = BuckMutMap::default();
        let mut dirs = BuckMutMap::default();
        for (i, set) in resolved.watch.iter().enumerate() {
            for f in &set.files {
                owners.insert(PathBuf::from(f), Owner::File(i));
            }
            for d in &set.dirs {
                dirs.insert(PathBuf::from(d), i);
            }
        }
        for f in &resolved.graph_files {
            owners.entry(PathBuf::from(f)).or_insert(Owner::Graph);
        }
        let mut watch_dirs = BTreeSet::new();
        for path in owners.keys() {
            if let Some(parent) = path.parent() {
                watch_dirs.insert(parent.to_path_buf());
            }
        }
        watch_dirs.extend(dirs.keys().cloned());

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
            resolved,
            owners,
            dirs,
            _watcher: watcher,
        })
    }

    /// The watch set of the nearest package directory that contains `path`.
    fn package_set(&self, path: &Path) -> Option<usize> {
        path.ancestors()
            .skip(1)
            .find_map(|dir| self.dirs.get(dir).copied())
    }

    fn notify_files(&self) -> Vec<PathBuf> {
        self.owners
            .iter()
            .filter(|(_, owner)| match owner {
                Owner::File(i) => self.resolved.watch[*i].notify,
                Owner::Graph => false,
            })
            .map(|(path, _)| path.clone())
            .collect()
    }
}

/// The longest `buck2 dev` waits for files to stop changing before it builds.
const MAX_DEBOUNCE: Duration = Duration::from_secs(2);

type Hashes = Arc<Mutex<BuckMutMap<PathBuf, Option<blake3::Hash>>>>;

fn hash_file(path: &Path) -> Option<blake3::Hash> {
    std::fs::read(path).ok().map(|bytes| blake3::hash(&bytes))
}

/// Hash `files` on another thread and record them in `hashes`, keeping hashes already there.
fn hash_in_background(files: Vec<PathBuf>, hashes: Hashes) {
    std::thread::spawn(move || {
        for path in files {
            let hash = hash_file(&path);
            if let Ok(mut hashes) = hashes.lock() {
                hashes.entry(path).or_insert(hash);
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
    server: Server,
    generation: u64,
    /// The digest of each output when it was last built.
    digests: BuckMutMap<String, Option<String>>,
    /// The content of each file of a `notify` set when the server last asked for a build.
    baseline: Hashes,
    /// The content of each changed file when it was last handled.
    last_seen: BuckMutMap<PathBuf, Option<blake3::Hash>>,
    /// Files of `notify` sets that differ from `baseline`.
    pending: BTreeSet<PathBuf>,
    /// Watch sets with changes that were not built because `pending` is not empty, and the
    /// files that changed in them.
    held: BTreeMap<usize, Vec<PathBuf>>,
}

impl<'a> Session<'a> {
    async fn start(mut daemon: Daemon<'a>, debounce: Duration) -> buck2_error::Result<Session<'a>> {
        let resolved = match daemon.resolve().await? {
            Ok(resolved) => resolved,
            Err(Failed(messages)) => {
                return Err(buck2_error!(
                    ErrorTag::Input,
                    "`buck2 dev` could not start: {}",
                    messages.join("\n")
                ));
            }
        };
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let watched = Watched::new(resolved, events_tx.clone())?;
        let baseline: Hashes = Arc::new(Mutex::new(BuckMutMap::default()));
        hash_in_background(watched.notify_files(), baseline.clone());
        let mut server = Server::spawn(&watched.resolved)?;
        server
            .send(json!({"type": "ready", "outputs": watched.resolved.outputs}))
            .await?;
        Ok(Session {
            daemon,
            debounce,
            events_tx,
            events_rx,
            watched,
            server,
            generation: 0,
            digests: BuckMutMap::default(),
            baseline,
            last_seen: BuckMutMap::default(),
            pending: BTreeSet::new(),
            held: BTreeMap::new(),
        })
    }

    async fn run(mut self) -> buck2_error::Result<ExitResult> {
        loop {
            tokio::select! {
                event = self.events_rx.recv() => {
                    let Some(event) = event else { continue };
                    let mut paths = BTreeSet::new();
                    self.collect_paths(event, &mut paths);
                    if paths.is_empty() {
                        continue;
                    }
                    // Wait until the watched files stop changing, but no longer than
                    // `MAX_DEBOUNCE`, in case they never do.
                    let deadline = tokio::time::Instant::now() + MAX_DEBOUNCE;
                    let mut quiet_until = tokio::time::Instant::now() + self.debounce;
                    loop {
                        let wait_until = quiet_until.min(deadline);
                        match tokio::time::timeout_at(wait_until, self.events_rx.recv()).await {
                            Ok(Some(event)) => {
                                let before = paths.len();
                                self.collect_paths(event, &mut paths);
                                if paths.len() != before {
                                    quiet_until = tokio::time::Instant::now() + self.debounce;
                                }
                            }
                            _ => break,
                        }
                    }
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

    /// Add the paths of `event` that belong to a watch set, a package or the graph.
    fn collect_paths(&self, event: notify::Result<notify::Event>, paths: &mut BTreeSet<PathBuf>) {
        let Ok(event) = event else { return };
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        for path in event.paths {
            if self.watched.owners.contains_key(&path) || self.watched.package_set(&path).is_some()
            {
                paths.insert(path);
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
                self.build(outputs, Vec::new()).await?;
                self.reset_pending().await?;
            }
            Ok(Request::Restart) => {
                self.restart().await?;
                self.reset_pending().await?;
            }
            Err(_) => {
                buck2_client_ctx::println!("{}", line)?;
            }
        }
        Ok(())
    }

    /// The server asked for a build or restart, so it now uses the files as they are: they are
    /// the new baseline.
    async fn reset_pending(&mut self) -> buck2_error::Result<()> {
        if let Ok(mut baseline) = self.baseline.lock() {
            for path in &self.pending {
                baseline.insert(path.clone(), hash_file(path));
            }
        }
        self.held.clear();
        if !self.pending.is_empty() {
            self.pending.clear();
            self.server
                .send(json!({"type": "stale", "files": Vec::<String>::new()}))
                .await?;
        }
        Ok(())
    }

    async fn on_changes(&mut self, paths: BTreeSet<PathBuf>) -> buck2_error::Result<()> {
        let mut regraph = false;
        let mut changed_sets: BTreeMap<usize, Vec<PathBuf>> = BTreeMap::new();
        let mut notify_changed: Vec<PathBuf> = Vec::new();
        let mut graph_changed: Vec<PathBuf> = Vec::new();

        for path in paths {
            let owner = self.watched.owners.get(&path).copied();
            let package_set = self.watched.package_set(&path);
            if owner.is_none() && package_set.is_none() {
                continue;
            }
            let hash = hash_file(&path);
            if let Some(Owner::File(i)) = owner {
                if self.watched.resolved.watch[i].notify {
                    notify_changed.push(path);
                    continue;
                }
            }
            if self.last_seen.get(&path) == Some(&hash) {
                continue;
            }
            self.last_seen.insert(path.clone(), hash);
            match owner {
                Some(Owner::File(i)) => {
                    if is_build_file(&path) {
                        regraph = true;
                    }
                    changed_sets.entry(i).or_default().push(path);
                }
                Some(Owner::Graph) => {
                    regraph = true;
                    graph_changed.push(path);
                }
                None => {
                    // A file added to or removed from a package: the package may have new
                    // sources.
                    regraph = true;
                    if let Some(i) = package_set {
                        changed_sets.entry(i).or_default().push(path);
                    }
                }
            }
        }

        // Track files of `notify` sets that differ from what the server last built.
        let before = self.pending.clone();
        for path in notify_changed {
            let hash = hash_file(&path);
            let baseline = self
                .baseline
                .lock()
                .ok()
                .and_then(|b| b.get(&path).copied());
            if baseline == Some(hash) {
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

        if regraph {
            self.reresolve(true).await?;
        }

        // A changed `.bzl` file may change any target, so it counts as a change to every set.
        if !graph_changed.is_empty() {
            for i in 0..self.watched.resolved.watch.len() {
                let set = &self.watched.resolved.watch[i];
                if set.notify {
                    self.pending.extend(graph_changed.iter().cloned());
                } else {
                    changed_sets
                        .entry(i)
                        .or_default()
                        .extend(graph_changed.iter().cloned());
                }
            }
            if self.pending != before {
                self.server
                    .send(json!({"type": "stale", "files": self.pending}))
                    .await?;
            }
        }

        if !self.pending.is_empty() {
            // Building now would build the changed files of the `notify` sets too. Wait until
            // the server asks for a build, or the files go back to what they were.
            for (set, files) in changed_sets {
                self.held.entry(set).or_default().extend(files);
            }
            return Ok(());
        }
        let mut all = std::mem::take(&mut self.held);
        for (set, files) in changed_sets {
            all.entry(set).or_default().extend(files);
        }
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
            let Some(set) = self.watched.resolved.watch.get(i) else {
                continue;
            };
            outputs.extend(set.build.iter().cloned());
            restart |= set.restart;
        }
        if !outputs.is_empty() {
            self.build(outputs.into_iter().collect(), files).await?;
        }
        if restart {
            self.restart().await?;
        }
        Ok(())
    }

    async fn build(
        &mut self,
        outputs: Vec<String>,
        files: Vec<PathBuf>,
    ) -> buck2_error::Result<()> {
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
        match self.daemon.build(&outputs).await? {
            Ok(built) => {
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
            Err(Failed(messages)) => {
                self.server
                    .send(json!({
                        "type": "failed",
                        "gen": generation,
                        "error": messages.join("\n"),
                    }))
                    .await?;
            }
        }
        Ok(())
    }

    /// Resolve again, which also builds the server. Restarts the server if its command line
    /// changed. `graph_changed` says whether to tell the server that the graph may have changed.
    async fn reresolve(&mut self, graph_changed: bool) -> buck2_error::Result<bool> {
        match self.daemon.resolve().await? {
            Ok(resolved) => {
                let server_changed = resolved.server != self.server.argv;
                hash_in_background(
                    resolved
                        .watch
                        .iter()
                        .filter(|s| s.notify)
                        .flat_map(|s| s.files.iter().map(PathBuf::from))
                        .collect(),
                    self.baseline.clone(),
                );
                self.watched = Watched::new(resolved, self.events_tx.clone())?;
                if graph_changed {
                    self.server.send(json!({"type": "graph_changed"})).await?;
                }
                if server_changed {
                    self.respawn().await?;
                }
                Ok(true)
            }
            Err(Failed(messages)) => {
                self.generation += 1;
                self.server
                    .send(json!({
                        "type": "failed",
                        "gen": self.generation,
                        "error": messages.join("\n"),
                    }))
                    .await?;
                Ok(false)
            }
        }
    }

    /// Rebuild the server and start it again.
    async fn restart(&mut self) -> buck2_error::Result<()> {
        let argv = self.server.argv.clone();
        // `reresolve` starts the server again itself when its command line changed.
        if self.reresolve(false).await? && self.server.argv == argv {
            self.respawn().await?;
        }
        Ok(())
    }

    async fn respawn(&mut self) -> buck2_error::Result<()> {
        // Stop the old server first, so that the new one can use its resources, such as a port.
        self.server.stop().await;
        self.server = Server::spawn(&self.watched.resolved)?;
        self.digests.clear();
        self.server
            .send(json!({"type": "ready", "outputs": self.watched.resolved.outputs}))
            .await?;
        Ok(())
    }
}

fn is_build_file(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|n| n.to_str()),
        Some("BUCK" | "BUCK.v2" | "TARGETS" | "TARGETS.v2" | "PACKAGE" | "TARGETS.fixture")
    )
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
