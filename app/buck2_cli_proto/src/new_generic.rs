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
}

#[derive(Serialize, Deserialize)]
pub enum DevAction {
    /// Analyse the target and build the server.
    Server,
    /// Find what the watch sets claim, with the files as they are now, and what changed since
    /// an earlier answer. Expanding `DevWatch.inputs` may build what dynamic actions need.
    Watch(DevWatchRequest),
    /// Build the named `DevInfo.outputs`. The build stops if a file that it builds from changes,
    /// whose change does not wait.
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
    /// The server command line. Paths in it are relative to the project root.
    pub server: Vec<String>,
    /// The names of `DevInfo.outputs`.
    pub outputs: Vec<String>,
    /// `DevInfo.interactive`: the server gets the terminal.
    pub interactive: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct DevWatchRequest {
    /// Project-relative paths that changed, to say what each is.
    pub paths: Vec<String>,
    /// The `state` of an earlier answer, to say what changed since. `None` at the start, or when
    /// changes were missed.
    pub previous: Option<DevWatchState>,
    /// Also answer the digest of every file whose change waits, in `waiting`.
    pub waiting_digests: bool,
}

/// Digests that say whether the watch sets changed. `buck2 dev` keeps them only to send them
/// back.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevWatchState {
    pub sets: Vec<DevSetState>,
    /// The `.bzl` files that the graph loads, and their contents.
    pub bzl_digest: String,
    /// The build files of the graph and the `.bzl` files, and their contents.
    pub graph_digest: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevSetState {
    /// The targets that belong to the set.
    pub targets_digest: String,
    /// The source files that the set claims, and their contents.
    pub files_digest: String,
}

#[derive(Serialize, Deserialize)]
pub struct DevWatched {
    /// The `DevInfo.watch` sets, in order.
    pub watch: Vec<DevWatchSet>,
    pub state: DevWatchState,
    /// What each of the requested paths is.
    pub paths: Vec<DevPath>,
    /// With `waiting_digests`: every file whose change waits.
    pub waiting: Vec<DevPath>,
    /// The sets whose targets, or claimed files or their contents, changed since `previous`, and
    /// every set if a `.bzl` file changed. Empty without `previous`.
    pub changed_sets: Vec<usize>,
    /// A build file or a `.bzl` file of the graph, or a target, changed since `previous`, so the
    /// server may build differently.
    pub graph_changed: bool,
    /// Why some `inputs` could not be expanded, for example because building what a dynamic
    /// action needs failed. Their sets have the source files of the targets that own those
    /// inputs and of their dependencies instead. Empty when every input was expanded.
    pub incomplete: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct DevWatchSet {
    pub build: Vec<String>,
    pub restart: bool,
    pub notify: bool,
    /// How many source files the set claims.
    pub files: usize,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct DevPath {
    /// Project-relative.
    pub path: String,
    /// The watch set that claims the file, if one does.
    pub set: Option<usize>,
    /// A change to it waits (see the `buck2 dev` docs): it is a file of a `notify` set, a build
    /// file of a package with a target in one, or a `.bzl` file when there is a `notify` set.
    pub waits: bool,
    /// The digest of its contents as the daemon knows them, or `None` if it does not exist.
    pub digest: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct DevBuilt {
    pub outputs: BTreeMap<String, DevBuiltOutput>,
}

#[derive(Serialize, Deserialize)]
pub struct DevBuiltOutput {
    /// Absolute path of the output.
    pub path: String,
    /// The content digest of the output, if it is a file or a directory.
    pub digest: Option<String>,
}
