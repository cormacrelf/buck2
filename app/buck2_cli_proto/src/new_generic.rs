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
    /// Build the server, and expand the watch policy into files.
    Resolve,
    /// Build the named `DevInfo.outputs`.
    Build { outputs: Vec<String> },
}

#[derive(Serialize, Deserialize)]
pub enum DevResponse {
    Resolve(DevResolved),
    Build(DevBuilt),
}

#[derive(Serialize, Deserialize)]
pub struct DevResolved {
    /// Absolute path of the project root. The server runs in this directory.
    pub project_root: String,
    /// The server command line. Paths in it are relative to the project root.
    pub server: Vec<String>,
    /// The names of `DevInfo.outputs`.
    pub outputs: Vec<String>,
    /// The `DevInfo.watch` sets, in order, with their files.
    pub watch: Vec<DevWatchSet>,
    /// Absolute paths of the `.bzl` files that the packages in the dependency graph load.
    pub graph_files: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct DevWatchSet {
    pub build: Vec<String>,
    pub restart: bool,
    pub notify: bool,
    /// Absolute paths of the source files and build files that belong to this set.
    pub files: Vec<String>,
    /// Absolute paths of the package directories that belong to this set. A file added to one
    /// of them may be a new source file of the set.
    pub dirs: Vec<String>,
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
