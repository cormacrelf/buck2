/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use buck2_fs::paths::abs_path::AbsPathBuf;
use serde::Deserialize;
use serde::Serialize;

use crate::TargetCfg;

#[derive(Serialize, Deserialize)]
pub enum NewGenericRequest {
    DebugEval(DebugEvalRequest),
    DebugAnonTargets(AnonTargetsRequest),
    Explain(ExplainRequest),
    ExpandExternalCells(ExpandExternalCellsRequest),
    Complete(CompleteRequest),
    Docs(DocsRequest),
}

#[derive(Serialize, Deserialize)]
pub enum NewGenericResponse {
    DebugEval(DebugEvalResponse),
    DebugAnonTargets(AnonTargetsResponse),
    Explain(ExplainResponse),
    ExpandExternalCells(ExpandExternalCellsResponse),
    Complete(CompleteResponse),
    Docs(DocsResponse),
}

/// What a command sends before its response.
#[derive(Serialize, Deserialize)]
pub enum NewGenericPartialResult {
    /// What happened in a `buck2 dev` session.
    DevSession(DevSessionEvent),
}

#[derive(Serialize, Deserialize)]
pub struct DebugEvalRequest {
    pub paths: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct DebugEvalResponse {}

#[derive(Serialize, Deserialize)]
pub struct AnonTargetsRequest {
    pub patterns: Vec<String>,
    pub target_cfg: TargetCfg,
    /// Also report which analyses requested each anon target.
    pub with_requesters: bool,
    pub json: bool,
    /// Regexes selecting the attributes to output (cquery `--output-attribute` semantics:
    /// search mode, `buck.`-prefixed special attributes included). Non-empty implies JSON.
    pub output_attributes: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct AnonTargetsResponse {
    /// The rendered output, printed by the client.
    pub serialized: String,
}

#[derive(Serialize, Deserialize)]
pub struct ExplainRequest {
    pub output: Option<AbsPathBuf>,
    pub target: String,
    pub fbs_dump: Option<AbsPathBuf>,
    pub manifold_path: Option<String>,
    pub log_path: AbsPathBuf,
    // build options
    pub target_universe: Vec<String>,
    pub target_cfg: TargetCfg,
}

#[derive(Serialize, Deserialize)]
pub struct ExplainResponse {}

#[derive(Serialize, Deserialize)]
pub enum ExpandExternalCellsRequest {
    All,
    Specific(BTreeSet<String>),
}

#[derive(Serialize, Deserialize)]
pub struct ExpandExternalCellsResponse {
    pub paths: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
pub struct CompleteRequest {
    pub target_cfg: TargetCfg,
    /// The label with partial target we want to complete \[\[cell\]//\]package:\[partial_target\]
    pub partial_target: String,
}

#[derive(Serialize, Deserialize)]
pub struct CompleteResponse {
    /// Completions matching the partial input.
    pub completions: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub enum DocsOutputFormat {
    Json,
    /// Contains the markdown output path
    Markdown(AbsPathBuf),
}

#[derive(Serialize, Deserialize)]
pub struct DocsStarlarkRequest {
    pub symbol_patterns: Vec<String>,
    pub format: DocsOutputFormat,
}

#[derive(Serialize, Deserialize)]
pub struct DocsStarlarkBuiltinsRequest {
    pub path: String,
}

#[derive(Serialize, Deserialize)]
pub enum DocsRequest {
    Starlark(DocsStarlarkRequest),
    StarlarkBuiltins(DocsStarlarkBuiltinsRequest),
}

#[derive(Serialize, Deserialize)]
pub enum DocsResponse {
    /// The command wrote its documentation directly to files.
    NoOutput,
    /// JSON documentation to print on stdout.
    Json(String),
}

/// A request of a `buck2 dev` session. A session is one daemon command, which lasts as long as
/// `buck2 dev` does. The first request is `Start`.
#[derive(Serialize, Deserialize)]
pub enum DevSessionRequest {
    Start(DevSessionStart),
    /// Define these dynamic outputs, or define them anew. `Build` then names them.
    Define(Vec<DevDynamicCall>),
    /// These dynamic outputs are no longer defined.
    Undefine(Vec<String>),
    /// Run `action`. The events about it carry `id`.
    ///
    /// `epoch` says which files the action must see. Actions of one epoch run against the same
    /// files, side by side. One of a later epoch waits until those of earlier epochs are done,
    /// and sees the files as they are when it starts. `buck2 dev` starts a new epoch once the
    /// file watcher has reported a change.
    Run {
        id: u64,
        epoch: u64,
        action: DevAction,
    },
}

/// What a `buck2 dev` session runs.
#[derive(Serialize, Deserialize)]
pub struct DevSessionStart {
    /// The target whose `DevInfo` to use, or, with `bxl_args`, a bxl function, `path.bxl:name`,
    /// whose returned providers to use in its place.
    pub target: String,
    /// When `target` is a bxl function, its command line arguments.
    pub bxl_args: Option<Vec<String>>,
    pub target_cfg: TargetCfg,
    /// The name of the session, as it gave it when it subscribed to file changes. The daemon
    /// leaves what the target is built from under this name, for the subscription to drop the
    /// changes that cannot matter.
    pub session: String,
}

#[derive(Serialize, Deserialize)]
pub enum DevAction {
    /// Analyse the target and build the server.
    Server,
    /// Say what the changed paths, and the changes to the targets since an earlier answer, are
    /// to the server and to each output. Finding what they may be built from, when the graph
    /// changed, may build what dynamic actions need to say what they do.
    Watch(DevWatchRequest),
    /// Build these outputs: names in `DevInfo.outputs`, and defined dynamic outputs. Each output
    /// is built on its own, and gets a `DevSessionEvent::Built` as soon as it is done. A build
    /// stops when a file that it is built from changes.
    Build { outputs: Vec<String> },
    /// Stop every build, release the local resources that the daemon holds for the session, and
    /// wait until they are gone. `buck2 dev` sends it after it has stopped the server.
    Release,
}

#[derive(Serialize, Deserialize)]
pub enum DevResponse {
    Server(DevServer),
    Watch(DevWatched),
    Released,
}

/// What happened in a `buck2 dev` session.
#[derive(Serialize, Deserialize)]
pub enum DevSessionEvent {
    /// The answer to a `Server`, `Watch` or `Release` request.
    Response { id: u64, response: DevResponse },
    /// A `Server` or `Watch` request failed, with these errors.
    Failed { id: u64, errors: Vec<String> },
    /// Output `name` of build `id` is done.
    Built {
        id: u64,
        name: String,
        result: Result<DevBuiltOutput, DevOutputError>,
    },
    /// What dynamic output `name` of build `id` may be built from, as the build found it: the
    /// state that the next `Watch` compares with. It comes after the output's `Built`, or not at
    /// all if it could not be found.
    Found {
        id: u64,
        name: String,
        state: DevWatchState,
    },
    /// Files that output `name` of build `id` may be built from changed after the build saw the
    /// files: `paths`, or anything if `missed`. With `stopped`, the build had not finished, so it
    /// stopped, and no `Built` follows. Otherwise the build was done, and used the files as
    /// they were before.
    Changed {
        id: u64,
        name: String,
        paths: Vec<String>,
        missed: bool,
        stopped: bool,
    },
}

/// Why an output did not build.
#[derive(Clone, Serialize, Deserialize)]
pub struct DevOutputError {
    pub error: String,
    /// An action failed, which the console has shown.
    pub action: bool,
}

#[derive(Default, Serialize, Deserialize)]
pub struct DevServer {
    /// Absolute path of the project root. The server runs in this directory.
    pub project_root: String,
    /// The server command line. Paths in it are absolute.
    pub server: Vec<String>,
    /// A digest of the contents of the artifacts that the server command line names, hidden
    /// ones too, which is the same in another daemon. A server that runs is out of date once
    /// this differs from what it was started with.
    pub digest: String,
    /// The names of `DevInfo.outputs`.
    pub outputs: Vec<String>,
    /// The names of `DevInfo.scopes`.
    pub scopes: Vec<String>,
    /// The names of `DevInfo.handlers`.
    #[serde(default)]
    pub handlers: Vec<String>,
    /// `DevInfo.interactive`: the server gets the terminal.
    pub interactive: bool,
    /// The environment variables that the local resources give the server.
    pub env: Vec<(String, String)>,
    /// One name for each local resource as it runs now. A resource that was set up anew has a
    /// new name, so a server that was started before it is out of date.
    pub resources: Vec<String>,
    /// The local resources that had exited by themselves, and that the daemon set up again.
    pub resources_exited: Vec<String>,
    /// Why local resources could not be set up, one message for each. The server must not run
    /// without them.
    pub resource_errors: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct DevWatchRequest {
    /// Project-relative paths that changed, to say what each is.
    pub paths: Vec<String>,
    /// The `state` of an earlier answer, to say what changed since. `None` at the start.
    pub previous: Option<DevWatchState>,
    /// The file watcher missed changes since `previous`, so anything may have changed.
    pub missed: bool,
    /// The dynamic outputs that the server tracks, each with the `state` of its last answer.
    pub dynamic: Vec<DevDynamicWatch>,
}

/// An output that the server defined at run time: a call of a handler of `DevInfo.handlers`, a
/// bxl function, with arguments.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct DevDynamicCall {
    /// The name that the server gave the output.
    pub name: String,
    /// The name of the handler in `DevInfo.handlers`.
    pub handler: String,
    /// The command line arguments of the bxl function.
    pub args: Vec<String>,
}

/// A defined dynamic output, to say what changes are to it.
#[derive(Clone, Serialize, Deserialize)]
pub struct DevDynamicWatch {
    pub name: String,
    /// The `state` of the last answer about it, or of the build that last built it. `None` if
    /// there was none, or the handler failed then.
    pub previous: Option<DevWatchState>,
}

/// What a `Watch` says of a dynamic output, besides its changes, which are in
/// `DevOutputChanges.outputs` under its name.
#[derive(Serialize, Deserialize)]
pub struct DevDynamicWatched {
    /// `None` if the handler, or finding what the output is built from, failed.
    pub state: Option<DevWatchState>,
    /// A build file or a `.bzl` file of its graph, or a target, changed since `previous`.
    pub graph_changed: bool,
}

/// Digests that say whether the targets of the server and the outputs, or the graph, changed.
/// `buck2 dev` keeps them only to send them back. They say nothing of the contents of source
/// files: the changed paths do.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevWatchState {
    /// The targets that the server and the outputs may be built from, in groups. Sorted.
    pub targets_digests: Vec<DevTargets>,
    /// The `.bzl` files that the graph loads, and their contents.
    pub bzl_digest: String,
    /// The build files of the graph and the `.bzl` files, and their contents.
    pub graph_digest: String,
}

/// The targets that the server or an output may be built from that are in the same scopes.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DevTargets {
    /// 0 for the server, and `i + 1` for the `i`th output.
    pub slot: usize,
    /// The scopes that the targets are in, as bits by the order of `DevInfo.scopes`.
    pub scopes: u64,
    /// A digest of the targets.
    pub digest: String,
}

#[derive(Serialize, Deserialize)]
pub struct DevWatched {
    pub state: DevWatchState,
    /// A build file or a `.bzl` file of the graph, or a target, changed since `previous`, so the
    /// server may build differently.
    pub graph_changed: bool,
    /// Why `buck2 dev` could not find everything that the server or some outputs are built
    /// from, for example because building what a dynamic action needs failed. They have the
    /// source files of the targets that own their artifacts and of their dependencies instead.
    /// Empty when everything was found.
    pub incomplete: Vec<String>,
    /// What the requested paths, and the changes since `previous`, are to the server and to
    /// each output, dynamic outputs too.
    pub outputs: DevOutputChanges,
    /// Each of the requested dynamic outputs, by name.
    #[serde(default)]
    pub dynamic: BTreeMap<String, DevDynamicWatched>,
}

/// What changes are to the server and to the outputs. Nothing here is exclusive: one path may be
/// here for the server and for several outputs.
#[derive(Default, Serialize, Deserialize)]
pub struct DevOutputChanges {
    /// The server's command may be built from a requested path, or its targets changed, so it
    /// may be new.
    pub server: bool,
    /// The outputs that the changes may affect, each with those changes: the requested paths
    /// that the output may be built from, and, if its targets changed, a change for them. With
    /// `missed` and a `previous`, every output has a change of the empty path, which stands for
    /// the project.
    pub outputs: BTreeMap<String, Vec<DevChange>>,
    /// How many targets the server and the outputs may be built from.
    pub targets: usize,
}

/// A change that an output may be built from.
#[derive(Serialize, Deserialize)]
pub struct DevChange {
    /// The project-relative path that changed: a source file, a build file or a `.bzl` file.
    /// `None` for a change to the targets that names no file, such as a `.buckconfig` change.
    pub path: Option<String>,
    /// The names of the scopes that the change is in.
    pub scopes: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct DevBuiltOutput {
    /// Absolute path of the output, for an output that is an artifact.
    #[serde(default)]
    pub path: Option<String>,
    /// For an output that is a command line: the command line, resolved as `DevServer.server`
    /// is, with absolute paths.
    #[serde(default)]
    pub argv: Option<Vec<String>>,
    /// The content digest of the output, if it is a file or a directory. For an output with
    /// associated artifacts, or a command line, a digest of the contents of all that it names,
    /// as `DevServer.digest`.
    pub digest: Option<String>,
    /// The sidecars of the output, by name.
    #[serde(default)]
    pub sidecars: BTreeMap<String, DevBuiltSidecar>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct DevBuiltSidecar {
    /// Absolute path of the sidecar.
    pub path: String,
    /// The content digest of the sidecar, if it is a file or a directory.
    pub digest: Option<String>,
}
