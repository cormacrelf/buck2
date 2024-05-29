/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

//! A build-system independent project structure defined and used by rust-analyzer.
//!
//! While this format is provisional and subject to change, `rust-project` and Rusty
//! take a necessary dependency on these definitions to provide IDE functionality to
//! buck-based projects. For additional details, see rust-analyzer's [documentation].
//!
//! [documentation]: https://rust-analyzer.github.io/manual.html#non-cargo-based-projects

use std::path::PathBuf;

use rustc_hash::FxHashMap;
use rustc_hash::FxHashSet;
use serde::Deserialize;
use serde::Serialize;

use crate::target::Target;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct JsonProject {
    #[serde(flatten)]
    pub(crate) sysroot: Sysroot,

    /// The set of crates comprising the project.
    ///
    /// Must include all transitive dependencies as well as sysroot crate (libstd,
    /// libcore, etc.).
    pub(crate) crates: Vec<Crate>,
    pub(crate) generated: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Crate {
    /// Optional crate name used for display purposes; has no semantic significance.
    pub(crate) display_name: Option<String>,
    /// The path to the root module of the crate.
    pub(crate) root_module: PathBuf,
    pub(crate) buck_extensions: BuckExtensions,
    pub(crate) edition: Edition,
    pub(crate) deps: Vec<Dep>,
    /// Should this crate be treated as a member of
    /// current "workspace".
    ///
    /// By default, inferred from the `root_module`
    /// (members are the crates which reside inside
    /// the directory opened in the editor).
    ///
    /// Set this to `false` for things like standard
    /// library and 3rd party crates to enable
    /// performance optimizations (rust-analyzer
    /// assumes that non-member crates don't change).
    pub(crate) is_workspace_member: bool,
    /// Optionally specify the (super)set of `.rs`
    /// files comprising this crate.
    ///
    /// By default, rust-analyzer assumes that only
    /// files under `root_module.parent` can belong
    /// to a crate. `include_dirs` are included
    /// recursively, unless a subdirectory is in
    /// `exclude_dirs`.
    ///
    /// Different crates can share the same `source`.
    ///
    /// If two crates share an `.rs` file in common,
    /// they *must* have the same `source`.
    /// rust-analyzer assumes that files from one
    /// source can't refer to files in another source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) source: Option<Source>,
    /// The set of cfgs activated for a given crate.
    ///
    /// With how fb imports crates into fbsource/third-party,
    /// the answer is "all of them".
    pub(crate) cfg: Vec<String>,
    /// The target triple for a given crate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) build_info: Option<BuildInfo>,
    /// Environment for the crate, often used by `env!`.
    pub(crate) env: FxHashMap<String, String>,
    /// Whether the crate is a proc-macro crate/
    pub(crate) is_proc_macro: bool,
    /// For proc-macro crates, path to compiled
    /// proc-macro (.so, .dylib, or .dll. depends on the platform.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) proc_macro_dylib_path: Option<PathBuf>,
}

/// Buck-specific extensions to the `rust-project.json` format.
///
/// This is needed to add support for reloading `rust-analyzer` when
/// a `TARGETS` file is changed.
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct BuckExtensions {
    /// Path corresponding to the BUCK defining the crate.
    pub(crate) build_file: PathBuf,
    /// A name corresponding to the Buck target of the crate.
    pub(crate) label: Target,
}

/// Build system-specific additions the `rust-project.json`.
///
/// rust-analyzer allows customising the flycheck command and various
/// runnables using pure shell. We hook into this to have rust-analyzer
/// call buck2 when you save files etc. Below is an example of `BuildInfo`
/// in JSON, which is a field on a crate in rust-project.json:
///
/// ```json
/// "build_info": {
///     "manifest_file": "/Users/dbarsky/fbsource/fbcode/buck2/integrations/rust-project/TARGETS",
///     "label": "//project/foo:my-crate",
///     "target_kind": "bin",
///     "shell_runnables": {
///         {
///             "kind": "check",
///             "program": "buck2",
///             "args": ["build", "//project/foo:my-crate"]
///         },
///         {
///             "kind": "run",
///             "program": "buck2",
///             "args": ["run", "//project/foo:my-crate"]
///         },
///         {
///             "kind": "test_one",
///             "program": "test_runner",
///             "args": ["--name=$$TEST_NAME$$"]
///         }
///     },
/// }
/// ```
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct BuildInfo {
    /// Buck extension
    ///
    /// `manifest_file` corresponds to the `BUCK`/`TARGETS` file.
    #[serde(default)]
    pub(crate) manifest_file: PathBuf,
    pub(crate) label: String,
    pub(crate) target_kind: TargetKind,
    pub(crate) shell_runnables: Vec<ShellRunnableArgs>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum TargetKind {
    #[default]
    Bin,
    /// Any kind of Cargo lib crate-type (dylib, rlib, proc-macro, ...).
    Lib,
    Example,
    Test,
    Bench,
    BuildScript,
    Other,
}

impl From<crate::target::Kind> for TargetKind {
    fn from(value: crate::target::Kind) -> Self {
        use crate::target::Kind::*;
        match value {
            Binary => TargetKind::Bin,
            Library => TargetKind::Lib,
            Test => TargetKind::Test,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShellRunnableArgs {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub kind: ShellRunnableKind,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ShellRunnableKind {
    Check,
    Run,
    TestOne,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Runnables {
    pub(crate) check: Vec<String>,
    pub(crate) run: Vec<String>,
    pub(crate) test: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq)]
#[serde(rename = "edition")]
pub(crate) enum Edition {
    #[serde(rename = "2015")]
    Edition2015,
    #[serde(rename = "2018")]
    Edition2018,
    #[default]
    #[serde(rename = "2021")]
    Edition2021,
}

/// An optional set of Rust files that comprise the crate.
///
/// By default, rust-analyzer assumes that only files under
/// `Crate::root_module` can belong to a crate. `include_dirs`
/// are included recursively, unless a subdirectory is
/// specified in `include_dirs`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct Source {
    pub(crate) include_dirs: FxHashSet<PathBuf>,
    pub(crate) exclude_dirs: FxHashSet<PathBuf>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Dep {
    #[serde(rename = "crate")]
    pub(crate) crate_index: usize,
    pub(crate) name: String,
}

/// Sysroot paths. These are documented in the rust-analyzer manual:
///
/// <https://rust-analyzer.github.io/manual.html#non-cargo-based-projects>
///
/// rust-analyzer treats both paths as optional, but we always provide sysroot.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sysroot {
    /// Path to the directory of the sysroot; this is a superset of `sysroot_src`.
    ///
    /// This path provides rust-analyzer both the *source code* of libraries
    /// like `std` and `core` and binaries like `rust-analyzer-proc-macro-srv`,
    /// which enable rust-analyzer to expand procedural macros.
    ///
    /// For example, a `sysroot` is `~/fbsource/fbcode/third-party-buck/platform010/build/rust/`.
    ///
    /// `rust-analyzer` relies on an external binary to expand procedural
    /// macros and the source code location can be predictably inferred.
    /// Assuming the example sysroot above, the source code would be located in
    /// `/lib/rustlib/src/rust/`.
    pub(crate) sysroot: PathBuf,
    /// Legacy sysroot config containing only the source code of libraries such
    /// as `std` and core`.
    ///
    /// Inside Meta, this is necessary on non-Linux platforms since the sources
    /// are packaged seperately from binaries such as `rust-analyzer-proc-macro-srv`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sysroot_src: Option<PathBuf>,
}
