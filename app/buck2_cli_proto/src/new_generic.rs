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
    Dev(DevRequest),
}

#[derive(Serialize, Deserialize)]
pub enum NewGenericResponse {
    DebugEval(DebugEvalResponse),
    DebugAnonTargets(AnonTargetsResponse),
    Explain(ExplainResponse),
    ExpandExternalCells(ExpandExternalCellsResponse),
    Complete(CompleteResponse),
    Docs(DocsResponse),
    Dev(DevResponse),
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

/// A request from `buck2 dev`. Each one is a separate, short daemon command, so `buck2 dev` holds
/// no daemon state between them.
#[derive(Serialize, Deserialize)]
pub struct DevRequest {
    /// The target whose `DevInfo` to use.
    pub target: String,
    pub target_cfg: TargetCfg,
    pub action: DevAction,
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
    /// Build the named `DevInfo.outputs`. The build stops if a file that it builds from changes.
    Build { outputs: Vec<String> },
}

#[derive(Serialize, Deserialize)]
pub enum DevResponse {
    Server(DevServer),
    Watch(DevWatched),
    Build(DevBuilt),
    /// The build stopped.
    Interrupted(DevInterrupted),
}

/// Why a `Build` stopped.
#[derive(Serialize, Deserialize)]
pub struct DevInterrupted {
    /// Project-relative paths of the files that changed.
    pub paths: Vec<String>,
    /// The file watcher missed changes, so anything may have changed.
    pub missed: bool,
}

#[derive(Serialize, Deserialize)]
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
    /// `DevInfo.interactive`: the server gets the terminal.
    pub interactive: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct DevWatchRequest {
    /// Project-relative paths that changed, to say what each is.
    pub paths: Vec<String>,
    /// The `state` of an earlier answer, to say what changed since. `None` at the start.
    pub previous: Option<DevWatchState>,
    /// The file watcher missed changes since `previous`, so anything may have changed.
    pub missed: bool,
}

/// Digests that say whether the targets of the server and the outputs, or the graph, changed.
/// `buck2 dev` keeps them only to send them back. They say nothing of the contents of source
/// files: the changed paths do.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevWatchState {
    /// A digest of each group of the targets that the server and the outputs may be built from.
    pub targets_digests: Vec<String>,
    /// The `.bzl` files that the graph loads, and their contents.
    pub bzl_digest: String,
    /// The build files of the graph and the `.bzl` files, and their contents.
    pub graph_digest: String,
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
    /// each output.
    pub outputs: DevOutputChanges,
}

/// What changes are to the server and to the outputs. Nothing here is exclusive: one path may be
/// here for the server and for several outputs.
#[derive(Default, Serialize, Deserialize)]
pub struct DevOutputChanges {
    /// The server's command may be built from a requested path, or its targets changed, so it
    /// may be new.
    pub server: bool,
    /// The outputs to build again, each with the requested paths that call for it: paths that
    /// the output may be built from, and, if its targets changed, the build files and `.bzl`
    /// files.
    pub build: BTreeMap<String, Vec<String>>,
    /// The outputs with `only` to hold back, each with the requested paths that call for it:
    /// the output may be built from them, and they are outside its `only`. With `missed` and
    /// a `previous`, every output is in `build` or here, the ones here with the empty path,
    /// which stands for the project.
    pub hold: BTreeMap<String, Vec<String>>,
    /// How many targets the server and the outputs may be built from.
    pub targets: usize,
}

#[derive(Serialize, Deserialize)]
pub struct DevBuilt {
    pub outputs: BTreeMap<String, DevBuiltOutput>,
    /// The outputs that failed to build, with their errors.
    #[serde(default)]
    pub failed: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
pub struct DevBuiltOutput {
    /// Absolute path of the output.
    pub path: String,
    /// The content digest of the output, if it is a file or a directory.
    pub digest: Option<String>,
    /// The sidecars of the output, by name.
    #[serde(default)]
    pub sidecars: BTreeMap<String, DevBuiltSidecar>,
}

#[derive(Serialize, Deserialize)]
pub struct DevBuiltSidecar {
    /// Absolute path of the sidecar.
    pub path: String,
    /// The content digest of the sidecar, if it is a file or a directory.
    pub digest: Option<String>,
}
