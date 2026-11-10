/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

//! The daemon side of `buck2 dev`.
//!
//! `buck2 dev` sends three kinds of request, each a separate daemon command:
//!
//! - `Server` builds the `DevInfo` server.
//! - `Watch` says what each changed path is to the watch sets, and whether the targets of the
//!   sets changed.
//! - `Build` builds named `DevInfo` outputs.
//!
//! The client runs the server, and acts on the file changes that the daemon's file watcher pushes
//! to it through a subscription, so no daemon command lasts longer than one build.
//!
//! What the watch sets claim is a DICE key, `DevClaimsKey`. Finding it walks the targets of the
//! graph, and the actions of the targets that own `DevWatch.inputs`, which builds what dynamic
//! actions of those targets need to say what they do. It reads the contents of no source file,
//! so DICE keeps it while only source files change: a `Watch` or a `Build` then looks up the
//! changed paths in it, and what that costs does not grow with the graph. Its value says which
//! set has which target, and lists no source files; see `DevClaims`.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::VecDeque;
use std::hash::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::sync::Arc;
use std::sync::Mutex;

use allocative::Allocative;
use async_trait::async_trait;
use buck2_artifact::actions::key::ActionKey;
use buck2_artifact::artifact::artifact_dyn::ArtifactDyn;
use buck2_artifact::artifact::artifact_type::Artifact;
use buck2_build_api::actions::artifact::get_artifact_fs::GetArtifactFs;
use buck2_build_api::actions::calculation::ActionCalculation;
use buck2_build_api::analysis::calculation::RuleAnalysisCalculation;
use buck2_build_api::artifact_groups::ArtifactGroup;
use buck2_build_api::artifact_groups::ResolvedArtifactGroup;
use buck2_build_api::artifact_groups::TransitiveSetProjectionKey;
use buck2_build_api::interpreter::rule_defs::cmd_args::ArtifactPathMapperImpl;
use buck2_build_api::interpreter::rule_defs::cmd_args::CommandLineBuilder;
use buck2_build_api::interpreter::rule_defs::cmd_args::SimpleCommandLineArtifactVisitor;
use buck2_build_api::interpreter::rule_defs::provider::builtin::dev_info::DevInfo;
use buck2_build_api::interpreter::rule_defs::provider::builtin::dev_watch::DevWatchSpec;
use buck2_build_api::materialize::HasMaterializationQueueTracker;
use buck2_build_api::materialize::MaterializationAndUploadContext;
use buck2_build_api::materialize::materialize_and_upload_artifact_group;
use buck2_cli_proto::new_generic::DevAction;
use buck2_cli_proto::new_generic::DevBuilt;
use buck2_cli_proto::new_generic::DevBuiltOutput;
use buck2_cli_proto::new_generic::DevInterrupted;
use buck2_cli_proto::new_generic::DevPath;
use buck2_cli_proto::new_generic::DevRequest;
use buck2_cli_proto::new_generic::DevResponse;
use buck2_cli_proto::new_generic::DevServer;
use buck2_cli_proto::new_generic::DevSetState;
use buck2_cli_proto::new_generic::DevWatchRequest;
use buck2_cli_proto::new_generic::DevWatchSet;
use buck2_cli_proto::new_generic::DevWatchState;
use buck2_cli_proto::new_generic::DevWatched;
use buck2_common::dice::cells::HasCellResolver;
use buck2_common::file_ops::dice::DiceFileComputations;
use buck2_common::file_ops::metadata::FileType;
use buck2_common::file_ops::metadata::RawPathMetadata;
use buck2_common::file_ops::metadata::RawSymlink;
use buck2_common::io::IoProvider;
use buck2_common::package_listing::dice::DicePackageListingResolver;
use buck2_common::package_listing::resolver::PackageListingResolver;
use buck2_common::pattern::parse_from_cli::parse_patterns_from_cli_args;
use buck2_core::bzl::ImportPath;
use buck2_core::cells::CellResolver;
use buck2_core::cells::cell_path::CellPath;
use buck2_core::deferred::base_deferred_key::BaseDeferredKey;
use buck2_core::execution_types::executor_config::PathSeparatorKind;
use buck2_core::fs::i_project_rel_path::IProjectRelativePath;
use buck2_core::fs::project::ProjectRoot;
use buck2_core::fs::project_rel_path::ProjectRelativePath;
use buck2_core::fs::project_rel_path::ProjectRelativePathBuf;
use buck2_core::package::PackageLabel;
use buck2_core::pattern::pattern::ParsedPattern;
use buck2_core::pattern::pattern_type::ProvidersPatternExtra;
use buck2_core::pattern::pattern_type::TargetPatternExtra;
use buck2_core::provider::label::ConfiguredProvidersLabel;
use buck2_core::target::configured_target_label::ConfiguredTargetLabel;
use buck2_data::BuildResult;
use buck2_error::BuckErrorContext;
use buck2_error::internal_error;
use buck2_execute::artifact::fs::ExecutorFs;
use buck2_fs::paths::forward_rel_path::ForwardRelativePath;
use buck2_hash::BuckMutMap;
use buck2_hash::BuckMutSet;
use buck2_interpreter::load_module::InterpreterCalculation;
use buck2_node::nodes::configured::ConfiguredTargetNode;
use buck2_node::nodes::configured_frontend::ConfiguredTargetNodeCalculation;
use buck2_node::nodes::frontend::TargetGraphCalculation;
use buck2_node::target_calculation::ConfiguredTargetCalculation;
use buck2_server_ctx::ctx::FileChangeBatch;
use buck2_server_ctx::ctx::ServerCommandContextTrait;
use buck2_server_ctx::dev_interest::FileInterest;
use buck2_server_ctx::dev_interest::clear_interest;
use buck2_server_ctx::dev_interest::set_interest;
use buck2_server_ctx::dev_interest::stable_hash;
use buck2_server_ctx::global_cfg_options::global_cfg_options_from_client_context;
use buck2_server_ctx::partial_result_dispatcher::NoPartialResult;
use buck2_server_ctx::partial_result_dispatcher::PartialResultDispatcher;
use buck2_server_ctx::template::ServerCommandTemplate;
use buck2_server_ctx::template::run_server_command;
use dice::CancellationContext;
use dice::DiceComputations;
use dice::DiceTransaction;
use dice::EqualityBehavior;
use dice::Key;
use dice::NoValueSerialize;
use dice::ValueSerialize;
use dupe::Dupe;
use futures::StreamExt;
use futures::channel::oneshot;
use futures::stream::BoxStream;
use pagable::Pagable;
use pagable::pagable_typetag;

#[derive(Debug, buck2_error::Error)]
#[buck2(tag = Input)]
enum DevError {
    #[error("`buck2 dev` takes exactly one target, but `{0}` matched {1}")]
    NotOneTarget(String, usize),
    #[error("Target `{0}` has no `DevInfo` provider, so `buck2 dev` cannot use it")]
    NoDevInfo(ConfiguredProvidersLabel),
    #[error("Target `{target}` has no output named `{name}` in its `DevInfo`")]
    UnknownOutput {
        target: ConfiguredProvidersLabel,
        name: String,
    },
    #[error("Output `{0}` of the `DevInfo` produced no artifact")]
    EmptyOutput(String),
}

pub(crate) async fn dev_command(
    ctx: &dyn ServerCommandContextTrait,
    partial_result_dispatcher: PartialResultDispatcher<NoPartialResult>,
    req: DevRequest,
) -> buck2_error::Result<DevResponse> {
    // A build stops when a file that it builds from changes. Subscribe before the command syncs
    // the file watcher, so that no change falls between what the build sees and what the
    // subscription reports. A change in between is reported but is no change to the build.
    let changes = match &req.action {
        DevAction::Build { .. } => ctx.subscribe_to_file_changes().await.ok(),
        DevAction::Server | DevAction::Watch(_) => None,
    };
    run_server_command(
        DevServerCommand {
            req,
            changes: Mutex::new(changes),
        },
        ctx,
        partial_result_dispatcher,
    )
    .await
}

struct DevServerCommand {
    req: DevRequest,
    changes: Mutex<Option<BoxStream<'static, FileChangeBatch>>>,
}

#[async_trait]
impl ServerCommandTemplate for DevServerCommand {
    type StartEvent = buck2_data::DevCommandStart;
    type EndEvent = buck2_data::DevCommandEnd;
    type Response = DevResponse;
    type PartialResult = NoPartialResult;

    async fn command(
        &self,
        server_ctx: &dyn ServerCommandContextTrait,
        _partial_result_dispatcher: PartialResultDispatcher<Self::PartialResult>,
        mut ctx: DiceTransaction,
    ) -> buck2_error::Result<Self::Response> {
        let changes = self
            .changes
            .lock()
            .ok()
            .and_then(|mut changes| changes.take());
        let response = dev(server_ctx, &mut ctx, &self.req, changes).await;
        if response.is_err() && matches!(self.req.action, DevAction::Watch(_)) {
            // What fixes the target may be outside what it was built from.
            clear_interest(&self.req.session);
        }
        response
    }

    fn build_result(&self, _response: &Self::Response) -> Option<BuildResult> {
        Some(BuildResult {
            build_completed: true,
        })
    }
}

async fn dev(
    server_ctx: &dyn ServerCommandContextTrait,
    ctx: &mut DiceTransaction,
    req: &DevRequest,
    changes: Option<BoxStream<'static, FileChangeBatch>>,
) -> buck2_error::Result<DevResponse> {
    let label = configured_label(server_ctx, &mut ctx.ctx(), req).await?;
    let providers = ctx
        .ctx()
        .get_providers(&label)
        .await?
        .require_compatible()?;
    // Held across awaits, so this needs the owned form.
    let info = providers
        .builtin_provider_value::<DevInfo>()?
        .ok_or_else(|| DevError::NoDevInfo(label.dupe()))?;
    let outputs = info.as_ref().value().as_ref().outputs()?;
    match &req.action {
        DevAction::Server => {
            let server = {
                let mut dice = ctx.ctx();
                let artifact_fs = dice.get_artifact_fs().await?;
                let inputs = {
                    let mut visitor = SimpleCommandLineArtifactVisitor::new();
                    info.as_ref()
                        .value()
                        .as_ref()
                        .server_command_line()
                        .visit_artifacts(&mut visitor)?;
                    visitor.inputs
                };
                let ensured = dice
                    .try_compute_join(inputs, async |dice, input| {
                        materialize_and_upload_artifact_group(
                            dice,
                            &input,
                            MaterializationAndUploadContext::materialize(),
                            &dice
                                .per_transaction_data()
                                .get_materialization_queue_tracker(),
                        )
                        .await
                        .map(|value| (input, value))
                    })
                    .await
                    .buck_error_context("Failed to build the `buck2 dev` server")?;
                let path_separator = if cfg!(windows) {
                    PathSeparatorKind::Windows
                } else {
                    PathSeparatorKind::Unix
                };
                let executor_fs = ExecutorFs::new(&artifact_fs, path_separator);
                let mut args = Vec::<String>::new();
                let mapper = ArtifactPathMapperImpl::from(&ensured);
                let mut builder = CommandLineBuilder::new_with_options(
                    &mut args,
                    &mapper,
                    &executor_fs,
                    true,
                    None,
                );
                info.as_ref()
                    .value()
                    .as_ref()
                    .server_command_line()
                    .add_to_command_line(&mut builder)?;
                args
            };
            Ok(DevResponse::Server(DevServer {
                project_root: server_ctx.project_root().root().to_string(),
                server,
                outputs: outputs.keys().cloned().collect(),
                interactive: info.as_ref().value().as_ref().interactive(),
            }))
        }
        DevAction::Watch(request) => {
            let watch = info.as_ref().value().as_ref().watch()?;
            Ok(DevResponse::Watch(
                watch_files(&mut ctx.ctx(), &label, &watch, request, &req.session).await?,
            ))
        }
        DevAction::Build { outputs: names } => {
            let mut wanted: Vec<(String, Artifact)> = Vec::with_capacity(names.len());
            for name in names {
                let artifact = outputs.get(name).ok_or_else(|| DevError::UnknownOutput {
                    target: label.dupe(),
                    name: name.clone(),
                })?;
                wanted.push((name.clone(), artifact.dupe()));
            }
            let Some(changes) = changes else {
                return Ok(DevResponse::Build(
                    build(server_ctx.project_root(), &mut ctx.ctx(), wanted).await?,
                ));
            };
            let watch = info.as_ref().value().as_ref().watch()?;
            let claims = dev_claims(&mut ctx.ctx(), &label).await?;
            let io = ctx
                .ctx()
                .global_data()
                .get::<Arc<dyn IoProvider>>()
                .map_err(|_| internal_error!("The daemon has no I/O provider"))?
                .dupe();
            // Each of the two stops the other: the build when a file that it builds from
            // changes, and the wait for such a change when the build is done.
            let (built_tx, built_rx) = oneshot::channel::<()>();
            let (changed_tx, changed_rx) = oneshot::channel::<()>();
            let project_root = server_ctx.project_root();
            let watch = &watch;
            let claims = &claims;
            let mut dice = ctx.ctx();
            let (built, interrupted) = dice
                .compute2(
                    async move |dice| {
                        tokio::select! {
                            built = build(project_root, dice, wanted) => {
                                let _ignored = built_tx.send(());
                                Some(built)
                            }
                            // Dropping the build cancels it.
                            _ = changed_rx => None,
                        }
                    },
                    async move |dice| {
                        tokio::select! {
                            interrupted = changed_files(dice, changes, claims, watch, io) => {
                                let _ignored = changed_tx.send(());
                                Some(interrupted)
                            }
                            _ = built_rx => None,
                        }
                    },
                )
                .await;
            match (built, interrupted) {
                (Some(built), _) => Ok(DevResponse::Build(built?)),
                (None, Some(interrupted)) => Ok(DevResponse::Interrupted(interrupted)),
                (None, None) => Err(internal_error!(
                    "A `buck2 dev` build stopped without a file change"
                )),
            }
        }
    }
}

/// What a file is, to tell whether it changed.
#[derive(PartialEq, Eq)]
enum FileShape {
    File {
        digest: String,
        executable: bool,
    },
    /// The target of a symlink is not compared.
    Symlink,
    Directory,
}

impl FileShape {
    fn of<T>(metadata: RawPathMetadata<T>) -> FileShape {
        match metadata {
            RawPathMetadata::File(file) => FileShape::File {
                digest: file.digest.data().to_string(),
                executable: file.is_executable,
            },
            RawPathMetadata::Symlink { .. } => FileShape::Symlink,
            RawPathMetadata::Directory => FileShape::Directory,
        }
    }
}

/// Wait for a file that the build is built from to change from what this command saw, and say
/// which did. A file saved without a change, one that nothing builds from, and one whose change
/// waits are no change.
async fn changed_files(
    dice: &mut DiceComputations<'_>,
    mut changes: BoxStream<'static, FileChangeBatch>,
    claims: &DevClaims,
    watch: &[DevWatchSpec],
    io: Arc<dyn IoProvider>,
) -> DevInterrupted {
    while let Some(batch) = changes.next().await {
        if batch.missed {
            return DevInterrupted {
                paths: Vec::new(),
                missed: true,
            };
        }
        let mut changed = Vec::new();
        for path in batch.paths {
            if !claims.may_claim(&path) {
                continue;
            }
            // A path that cannot be looked at counts as changed.
            if builds_from_changed(dice, claims, watch, &io, &path)
                .await
                .unwrap_or(true)
            {
                changed.push(path.to_string());
            }
        }
        if !changed.is_empty() {
            return DevInterrupted {
                paths: changed,
                missed: false,
            };
        }
    }
    // The file watcher is gone, as when the daemon shuts down.
    futures::future::pending().await
}

/// Whether the build is built from `path`, and the file differs from what this command saw.
async fn builds_from_changed(
    dice: &mut DiceComputations<'_>,
    claims: &DevClaims,
    watch: &[DevWatchSpec],
    io: &Arc<dyn IoProvider>,
    path: &ProjectRelativePath,
) -> buck2_error::Result<bool> {
    let cell_resolver = dice.get_cell_resolver().await?;
    let relevant = match claims.classify(dice, cell_resolver, path).await? {
        Claimed::Source(set) => !watch[set].build.is_empty() || watch[set].restart,
        Claimed::BuildFile { waits } => !waits,
        Claimed::Bzl => !claims.notify_sets,
        Claimed::Nothing => false,
    };
    if !relevant {
        return Ok(false);
    }
    // What this command sees: what the build read, or will read. A file that the build has not
    // read yet is read now, and so is no change to it.
    let before = DiceFileComputations::read_path_metadata_if_exists(
        dice,
        cell_resolver.get_cell_path(&path),
    )
    .await?
    .map(FileShape::of);
    let now = io
        .read_path_metadata_if_exists_impl(IProjectRelativePath::intern_directly(path))
        .await?
        .map(FileShape::of);
    Ok(before != now)
}

async fn configured_label(
    server_ctx: &dyn ServerCommandContextTrait,
    dice: &mut DiceComputations<'_>,
    req: &DevRequest,
) -> buck2_error::Result<ConfiguredProvidersLabel> {
    let global_cfg_options =
        global_cfg_options_from_client_context(&req.target_cfg, server_ctx, dice).await?;
    let patterns = parse_patterns_from_cli_args::<ProvidersPatternExtra>(
        dice,
        std::slice::from_ref(&req.target),
        server_ctx.working_dir(),
    )
    .await?;
    let [pattern] = patterns.as_slice() else {
        return Err(DevError::NotOneTarget(req.target.clone(), patterns.len()).into());
    };
    let label = pattern.clone().as_providers_label(&req.target)?;
    dice.get_configured_provider_label(&label, &global_cfg_options)
        .await
}

/// Build each output on its own: one that fails does not keep the others from the server.
async fn build(
    project_root: &ProjectRoot,
    dice: &mut DiceComputations<'_>,
    wanted: Vec<(String, Artifact)>,
) -> buck2_error::Result<DevBuilt> {
    let artifact_fs = dice.get_artifact_fs().await?;
    let built = dice
        .compute_join(wanted, async |dice, (name, artifact)| {
            let values = materialize_and_upload_artifact_group(
                dice,
                &ArtifactGroup::Artifact(artifact.dupe()),
                MaterializationAndUploadContext::materialize(),
                &dice
                    .per_transaction_data()
                    .get_materialization_queue_tracker(),
            )
            .await;
            (name, values)
        })
        .await;
    let mut outputs = BTreeMap::new();
    let mut failed = BTreeMap::new();
    for (name, values) in built {
        let values = match values {
            Ok(values) => values,
            Err(e) => {
                failed.insert(name, output_error(&e));
                continue;
            }
        };
        let (artifact, value) = values
            .iter()
            .next()
            .ok_or_else(|| DevError::EmptyOutput(name.clone()))?;
        let path = artifact.resolve_path(&artifact_fs, Some(&value.content_based_path_hash()))?;
        outputs.insert(
            name,
            DevBuiltOutput {
                path: project_root.resolve(&path).to_string(),
                digest: value.digest().map(|d| d.to_string()),
            },
        );
    }
    Ok(DevBuilt { outputs, failed })
}

/// The most of an action's stdout or stderr that `output_error` keeps: the end, where the
/// errors usually are.
const MAX_ERROR_STREAM: usize = 16 << 10;

/// Why an output failed to build. For an action, that is what the console shows for it, with
/// its command, exit code and output, rather than only which target failed to build.
fn output_error(e: &buck2_error::Error) -> String {
    if let Some(action_error) = e.action_error() {
        let mut action_error = action_error.clone();
        if let Some(details) = action_error
            .last_command
            .as_mut()
            .and_then(|command| command.details.as_mut())
        {
            details.cmd_stdout = tail(&details.cmd_stdout);
            details.cmd_stderr = tail(&details.cmd_stderr);
        }
        if let Ok(display) = buck2_event_observer::display::display_action_error(
            &action_error,
            buck2_event_observer::display::TargetDisplayOptions::for_build_report(),
        ) {
            return display.simple_format_for_build_report();
        }
    }
    format!("{e:#}")
}

/// The end of `stream`, at most `MAX_ERROR_STREAM` bytes, saying how much it leaves out.
fn tail(stream: &str) -> String {
    if stream.len() <= MAX_ERROR_STREAM {
        return stream.to_owned();
    }
    let mut start = stream.len() - MAX_ERROR_STREAM;
    while !stream.is_char_boundary(start) {
        start += 1;
    }
    format!("[{start} bytes left out]\n{}", &stream[start..])
}

/// Logs how long the steps of finding the claims take, at the `debug` level: run the daemon
/// with `BUCK_LOG=buck2_server_commands::dev=debug`. `tests/dev_bench/bench.py` shows them.
struct Timer {
    start: std::time::Instant,
    last: std::time::Instant,
}

impl Timer {
    fn new() -> Timer {
        let now = std::time::Instant::now();
        Timer {
            start: now,
            last: now,
        }
    }

    /// Log that `step` is done, how long it took, and how long everything took so far.
    fn step(&mut self, step: &str) {
        let now = std::time::Instant::now();
        tracing::debug!(
            "buck2 dev: {step}: {:.1}ms ({:.1}ms so far)",
            (now - self.last).as_secs_f64() * 1000.0,
            (now - self.start).as_secs_f64() * 1000.0,
        );
        self.last = now;
    }
}

/// What the watch sets claim, before it is made small enough to keep.
///
/// Finding it walks the targets of the graph, not their source files and not the action graph:
/// a set claims the targets that its `targets` match, and the targets that its `inputs` are
/// built from, with all their source files. Which file that is comes from the targets when a
/// file changes; see `DevClaims::classify`. Only the actions of the targets that own the
/// `inputs` are looked at, so that a target that builds an input and other things does not give
/// the set the source files of the other things.
struct GraphClaims {
    /// The targets of the graph.
    nodes: Vec<ConfiguredTargetNode>,
    /// The source files that a set claims by name: those that the actions of the targets that
    /// own its `inputs` use, and those that claimed symlinks point at.
    files: BuckMutMap<ProjectRelativePathBuf, (usize, CellPath)>,
    /// The set of each target of the graph that a set has.
    targets: BuckMutMap<ConfiguredTargetLabel, usize>,
    /// The set that claims the own source files of each target.
    with_own_files: BuckMutMap<ConfiguredTargetLabel, usize>,
    /// The targets of each set.
    set_targets: Vec<Vec<ConfiguredTargetNode>>,
    /// The directory of each package of the graph.
    package_dirs: BTreeSet<ProjectRelativePathBuf>,
    /// Each build file of the graph, with whether a change to it waits.
    build_files: BTreeMap<ProjectRelativePathBuf, (bool, CellPath)>,
    /// The `.bzl` files that the graph loads.
    bzl_files: BTreeMap<ProjectRelativePathBuf, CellPath>,
    /// Whether there is a `notify` set. A change to a `.bzl` file then waits.
    notify_sets: bool,
    incomplete: Vec<String>,
}

impl GraphClaims {
    /// The set that claims the source file `path` of a target whose own source files `own`
    /// claims: the lower of that set and the one that claims the file by name.
    fn set_of_own_file(&self, cell_resolver: &CellResolver, path: CellPath, own: usize) -> usize {
        match project_path(cell_resolver, path) {
            Ok(Some(rel)) => match self.files.get(&rel) {
                Some((set, _)) => own.min(*set),
                None => own,
            },
            _ => own,
        }
    }
}

/// Walk the configured dependency graph of `label`. Give each target to the first watch set
/// that claims it.
///
/// This reads the contents of no source file, so that a `DevClaims` made from it stays valid
/// when one changes.
async fn graph_claims(
    dice: &mut DiceComputations<'_>,
    label: &ConfiguredProvidersLabel,
    watch: &[DevWatchSpec],
) -> buck2_error::Result<GraphClaims> {
    let mut timer = Timer::new();
    let cell_resolver = dice.get_cell_resolver().await?;
    let cell = label.target().pkg().cell_name();
    let cell_alias_resolver = dice.get_cell_alias_resolver(cell).await?;

    let mut patterns: Vec<Option<Vec<ParsedPattern<TargetPatternExtra>>>> =
        Vec::with_capacity(watch.len());
    for spec in watch {
        patterns.push(match &spec.targets {
            None => None,
            Some(targets) => Some(
                targets
                    .iter()
                    .map(|p| {
                        ParsedPattern::<TargetPatternExtra>::parse_precise(
                            p,
                            cell,
                            cell_resolver,
                            &cell_alias_resolver,
                        )
                    })
                    .collect::<buck2_error::Result<Vec<_>>>()?,
            ),
        });
    }

    let root = dice
        .get_configured_target_node(label.target())
        .await
        .require_compatible()?;
    let nodes = dependency_closure([root.dupe()]);
    let by_label: BuckMutMap<ConfiguredTargetLabel, ConfiguredTargetNode> = nodes
        .iter()
        .map(|node| (node.label().dupe(), node.dupe()))
        .collect();
    timer.step(&format!("the {} targets of the graph", nodes.len()));

    // The lowest watch set index that claims each target, and each source file that a set
    // claims by name. A set claims the targets that its `targets` match, with their own source
    // files. Of the targets that own its `inputs`, it claims only the source files that their
    // actions use. What those actions take from other targets gives it those targets and their
    // dependencies, with their own source files. The targets of a set say which sets a build
    // file change changed.
    let mut files: BuckMutMap<ProjectRelativePathBuf, (usize, CellPath)> = BuckMutMap::default();
    let mut targets: BuckMutMap<ConfiguredTargetLabel, usize> = BuckMutMap::default();
    let mut with_own_files: BuckMutMap<ConfiguredTargetLabel, usize> = BuckMutMap::default();
    let mut incomplete = Vec::new();
    for (set, spec) in watch.iter().enumerate() {
        for node in &nodes {
            let matches = match &patterns[set] {
                None => true,
                Some(p) => p.iter().any(|p| p.matches(node.label().unconfigured())),
            };
            if matches {
                claim(&mut targets, node.label().dupe(), set);
                claim(&mut with_own_files, node.label().dupe(), set);
            }
        }
        if spec.inputs.is_empty() {
            continue;
        }
        let walk = walk_inputs(dice, &spec.inputs).await;
        for path in walk.sources {
            claim_file(&mut files, cell_resolver, path, set)?;
        }
        for owner in walk.owners {
            if by_label.contains_key(&owner) {
                claim(&mut targets, owner, set);
            }
        }
        // What the inputs take from other targets can only be built from the source files of
        // those targets and their dependencies. So can an input that could not be expanded,
        // from those of its owner.
        let mut beyond: Vec<ConfiguredTargetNode> = walk
            .beyond
            .iter()
            .filter_map(|target| by_label.get(target))
            .map(|node| node.dupe())
            .collect();
        for (owner, error) in walk.failed {
            incomplete.push(format!("`DevInfo.watch[{set}].inputs`: {error:#}"));
            beyond.extend(
                owner
                    .and_then(|owner| by_label.get(&owner))
                    .map(|node| node.dupe()),
            );
        }
        for node in dependency_closure(beyond) {
            claim(&mut targets, node.label().dupe(), set);
            claim(&mut with_own_files, node.label().dupe(), set);
        }
    }
    timer.step("the targets of each set");

    let notify_sets = watch.iter().any(|spec| spec.notify);
    let mut set_targets: Vec<Vec<ConfiguredTargetNode>> = vec![Vec::new(); watch.len()];
    // Each build file, with whether a change to it waits.
    let mut build_files: BTreeMap<ProjectRelativePathBuf, (bool, CellPath)> = BTreeMap::new();
    let mut packages = BTreeSet::new();
    let mut package_dirs = BTreeSet::new();
    for node in &nodes {
        let set = targets.get(node.label()).copied();
        if let Some(set) = set {
            set_targets[set].push(node.dupe());
        }
        if packages.insert(node.label().pkg()) {
            if let Some(dir) = project_path(cell_resolver, node.label().pkg().as_cell_path())? {
                package_dirs.insert(dir);
            }
        }
        let buildfile = node.buildfile_path().path();
        if let Some(path) = project_path(cell_resolver, buildfile.dupe())? {
            build_files.entry(path).or_insert((false, buildfile)).0 |=
                set.is_some_and(|set| watch[set].notify);
        }
    }
    timer.step(&format!("the {} packages of the graph", packages.len()));

    let mut claims = GraphClaims {
        nodes,
        files,
        targets,
        with_own_files,
        set_targets,
        package_dirs,
        build_files,
        bzl_files: BTreeMap::new(),
        notify_sets,
        incomplete,
    };
    claim_symlink_targets(dice, cell_resolver, &packages, &mut claims).await?;
    timer.step("the symlinks in the packages");

    claims.bzl_files = loaded_files(dice, cell_resolver, packages).await?;
    // A build file changes the graph, even when a target also uses it as a source file.
    for path in claims.build_files.keys() {
        claims.files.remove(path);
    }
    timer.step("the `.bzl` files that the packages load");
    Ok(claims)
}

/// A source file that is a symlink is built from the file that it points at, which the set
/// claims too, by name.
///
/// The symlinks come from the listings of the directories of the packages, and not from the
/// source files of the targets: there are few symlinks, and many source files. Whether an entry
/// of a directory is a symlink depends on the contents of no file, and on the listing of the
/// directory only for which of its entries are symlinks.
async fn claim_symlink_targets(
    dice: &mut DiceComputations<'_>,
    cell_resolver: &CellResolver,
    packages: &BTreeSet<PackageLabel>,
    claims: &mut GraphClaims,
) -> buck2_error::Result<()> {
    let listings = dice
        .try_compute_join(packages.iter().copied(), async |dice, package| {
            buck2_error::Ok((
                package,
                DicePackageListingResolver(dice).resolve(package).await?,
            ))
        })
        .await?;
    // The directories of each package.
    let mut package_of: BuckMutMap<ProjectRelativePathBuf, PackageLabel> = BuckMutMap::default();
    for (package, listing) in &listings {
        let root = package.as_cell_path();
        if let Some(dir) = project_path(cell_resolver, root)? {
            package_of.insert(dir, *package);
        }
        for dir in listing.directories() {
            if let Some(dir) = project_path(cell_resolver, root.join(dir))? {
                package_of.insert(dir, *package);
            }
        }
    }
    let mut symlinks = Symlinks::default();
    symlinks
        .fill(dice, cell_resolver, package_of.keys().map(|dir| &**dir))
        .await?;

    // The source files that are symlinks, or behind one, with the set that claims them.
    let mut links: Vec<(usize, CellPath)> = Vec::new();
    // Those that a set claims by name.
    for (path, (set, cell_path)) in &claims.files {
        if symlinks.is_behind(dice, cell_resolver, path).await? {
            links.push((*set, cell_path.dupe()));
        }
    }
    // Those of the targets whose own source files a set claims: the symlinks in a package are
    // source files of the targets of that package.
    let mut linked: BTreeMap<PackageLabel, Vec<CellPath>> = BTreeMap::new();
    for (dir, names) in &symlinks.links {
        let Some(package) = package_of.get(dir) else {
            continue;
        };
        for name in names.iter() {
            let link = dir.join(ForwardRelativePath::new(name)?);
            linked
                .entry(*package)
                .or_default()
                .push(cell_resolver.get_cell_path(&link));
        }
    }
    for node in &claims.nodes {
        let (Some(linked), Some(own)) = (
            linked.get(&node.label().pkg()),
            claims.with_own_files.get(node.label()),
        ) else {
            continue;
        };
        for input in node.inputs() {
            if linked.iter().any(|link| input.starts_with(*link)) {
                links.push((claims.set_of_own_file(cell_resolver, input, *own), input));
            }
        }
    }

    for _ in 0..MAX_SYMLINK_HOPS {
        if links.is_empty() {
            break;
        }
        let targets = dice
            .try_compute_join(links, async |dice, (set, path)| {
                let target =
                    match DiceFileComputations::read_path_metadata_if_exists(dice, path).await? {
                        Some(RawPathMetadata::Symlink {
                            to: RawSymlink::Relative(target, _),
                            ..
                        }) => Some((set, target)),
                        _ => None,
                    };
                buck2_error::Ok(target)
            })
            .await?;
        links = Vec::new();
        for (set, target) in targets.into_iter().flatten() {
            let Some(rel) = project_path(cell_resolver, target.dupe())? else {
                continue;
            };
            let new = !claims.files.contains_key(&rel);
            claim_file(&mut claims.files, cell_resolver, target.dupe(), set)?;
            // Only a symlink is read again: reading a file would depend on its contents.
            if new && symlinks.is_behind(dice, cell_resolver, &rel).await? {
                links.push((set, target));
            }
        }
    }
    Ok(())
}

/// Which paths are symlinks, or are behind one, from the listings of the directories above them.
#[derive(Default)]
struct Symlinks {
    /// For each directory, whether it is a symlink or is behind one.
    behind: BuckMutMap<ProjectRelativePathBuf, bool>,
    /// The entries of each directory that are symlinks.
    links: BuckMutMap<ProjectRelativePathBuf, Arc<Vec<String>>>,
}

impl Symlinks {
    /// Find which of `dirs`, and of the directories above them, are symlinks or behind one, and
    /// the symlinks in the others. The directories of one depth are looked at together, after
    /// those above them, so that no directory behind a symlink is listed.
    async fn fill<'a>(
        &mut self,
        dice: &mut DiceComputations<'_>,
        cell_resolver: &CellResolver,
        dirs: impl Iterator<Item = &'a ProjectRelativePath>,
    ) -> buck2_error::Result<()> {
        let mut seen: BuckMutSet<&ProjectRelativePath> = BuckMutSet::default();
        let mut by_depth: Vec<Vec<&ProjectRelativePath>> = Vec::new();
        for dir in dirs {
            let mut above = Some(dir);
            while let Some(dir) = above {
                if self.behind.contains_key(dir) || !seen.insert(dir) {
                    break;
                }
                let depth = dir
                    .as_str()
                    .split('/')
                    .filter(|part| !part.is_empty())
                    .count();
                if by_depth.len() <= depth {
                    by_depth.resize_with(depth + 1, Vec::new);
                }
                by_depth[depth].push(dir);
                above = dir.parent();
            }
        }
        for dirs in by_depth {
            let mut listed = Vec::new();
            for dir in dirs {
                let behind = match (dir.parent(), dir.file_name()) {
                    (Some(parent), Some(name)) => {
                        self.behind.get(parent).copied().unwrap_or(false)
                            || self.links.get(parent).is_some_and(|links| {
                                links
                                    .binary_search_by(|link| link.as_str().cmp(name.as_str()))
                                    .is_ok()
                            })
                    }
                    // The project root.
                    _ => false,
                };
                self.behind.insert(dir.to_owned(), behind);
                if !behind && !self.links.contains_key(dir) {
                    listed.push(dir);
                }
            }
            let found = dice
                .try_compute_join(listed, async |dice, dir| {
                    let links = dice
                        .compute(&DevDirLinksKey(cell_resolver.get_cell_path(&dir)))
                        .await?
                        .dupe();
                    buck2_error::Ok((dir, links))
                })
                .await?;
            for (dir, links) in found {
                self.links.insert(dir.to_owned(), links);
            }
        }
        Ok(())
    }

    /// Whether `path` is a symlink, or a directory above it is.
    async fn is_behind(
        &mut self,
        dice: &mut DiceComputations<'_>,
        cell_resolver: &CellResolver,
        path: &ProjectRelativePath,
    ) -> buck2_error::Result<bool> {
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
            // The project root.
            return Ok(false);
        };
        if let Some(behind) = self.behind.get(path) {
            return Ok(*behind);
        }
        // The directories above `path` whose answer is not known yet, the highest last.
        let mut unknown: Vec<&ProjectRelativePath> = Vec::new();
        let mut above = Some(dir);
        while let Some(dir) = above {
            if self.behind.contains_key(dir) {
                break;
            }
            unknown.push(dir);
            above = dir.parent();
        }
        for dir in unknown.into_iter().rev() {
            let behind = match (dir.parent(), dir.file_name()) {
                (Some(parent), Some(name)) => {
                    self.behind.get(parent).copied().unwrap_or(false)
                        || self
                            .is_link(dice, cell_resolver, parent, name.as_str())
                            .await?
                }
                // The project root.
                _ => false,
            };
            self.behind.insert(dir.to_owned(), behind);
        }
        Ok(self.behind.get(dir).copied().unwrap_or(false)
            || self
                .is_link(dice, cell_resolver, dir, name.as_str())
                .await?)
    }

    /// Whether the entry `name` of `dir`, which is behind no symlink, is a symlink.
    async fn is_link(
        &mut self,
        dice: &mut DiceComputations<'_>,
        cell_resolver: &CellResolver,
        dir: &ProjectRelativePath,
        name: &str,
    ) -> buck2_error::Result<bool> {
        let links = match self.links.get(dir) {
            Some(links) => links,
            None => {
                let links = dice
                    .compute(&DevDirLinksKey(cell_resolver.get_cell_path(&dir)))
                    .await?
                    .dupe();
                self.links.entry(dir.to_owned()).or_insert(links)
            }
        };
        Ok(links
            .binary_search_by(|link| link.as_str().cmp(name))
            .is_ok())
    }
}

/// The names of the entries of a directory that are symlinks, sorted. Empty for a directory that
/// cannot be listed.
///
/// What the watch sets claim depends on this, and not on the listing: a file that is added to
/// the directory, or removed from it, then leaves the claims as they are.
#[derive(Clone, Dupe, Debug, Eq, Hash, PartialEq, Allocative, Pagable)]
#[pagable_typetag(dice::DiceKeyDyn)]
struct DevDirLinksKey(CellPath);

impl std::fmt::Display for DevDirLinksKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DevDirLinks({})", self.0)
    }
}

#[async_trait]
impl Key for DevDirLinksKey {
    type Value = Arc<Vec<String>>;

    async fn compute(
        &self,
        ctx: &mut DiceComputations,
        _cancellations: &CancellationContext,
    ) -> Self::Value {
        let links = match DiceFileComputations::read_dir(ctx, self.0).await {
            Ok(listing) => listing
                .included
                .iter_with_tag()
                .filter(|(_, file_type)| *file_type == FileType::Symlink)
                .map(|(name, _)| name.as_str().to_owned())
                .collect(),
            Err(_) => Vec::new(),
        };
        Arc::new(links)
    }

    fn equality_behavior() -> EqualityBehavior<Self::Value> {
        EqualityBehavior::Compare(|x, y| x == y)
    }

    fn value_serialize() -> impl ValueSerialize<Value = Self::Value> {
        NoValueSerialize::<Self::Value>::new()
    }
}

/// No watch set, in the tables of a `DevClaims`.
const NO_SET: u32 = u32::MAX;

/// What a path is to the watch sets.
enum Claimed {
    /// A source file that this set claims.
    Source(usize),
    /// A build file of the graph, and whether a change to it waits.
    BuildFile {
        waits: bool,
    },
    /// A `.bzl` file that the graph loads.
    Bzl,
    Nothing,
}

/// What the watch sets claim, small enough to keep: no file is here by name.
///
/// A set claims most files as the source files of its targets, and the targets already have
/// those, so this only says which set has which target. To find the set of a path, `classify`
/// looks at the targets of the packages that the path is in, as the `owner()` query does. Only
/// the few files that a set claims by name are kept, as hashes of their paths: those that the
/// actions of the targets that own its `inputs` use, and those that claimed symlinks point at.
///
/// This depends on the graph and not on the contents of source files, so DICE keeps it while
/// only those change, and makes it again when a target, a build file or a `.bzl` file does.
/// The exceptions are sets whose `inputs` are made by dynamic actions of the targets that own
/// them, which depend on what those read.
#[derive(Allocative, Debug, PartialEq, Eq)]
struct DevClaims {
    /// For each target of the graph, by the hash of its unconfigured label: the set that has it,
    /// and the set that claims its own source files. Sorted.
    targets: Vec<(u64, u32, u32)>,
    /// The source files that a set claims by name, by the hash of their project-relative path,
    /// with the set. Sorted.
    files: Vec<(u64, u32)>,
    /// The packages of the graph, and the files that matter outside them: what the session's
    /// file change subscription needs to drop the changes that cannot matter.
    interest: Arc<FileInterest>,
    /// The build files of the graph, with whether a change to them waits. Sorted.
    build_files: Vec<(u64, bool)>,
    /// The `.bzl` files that the graph loads. Sorted.
    bzl_files: Vec<u64>,
    /// For each set, a digest of its targets.
    targets_digests: Vec<String>,
    /// For each set, how many targets it has.
    target_counts: Vec<usize>,
    /// The `.bzl` files that the graph loads, and their contents.
    bzl_digest: String,
    /// The build files of the graph and the `.bzl` files, and their contents.
    graph_digest: String,
    notify_sets: bool,
    incomplete: Vec<String>,
}

impl DevClaims {
    async fn new(
        dice: &mut DiceComputations<'_>,
        claims: GraphClaims,
    ) -> buck2_error::Result<Self> {
        let mut timer = Timer::new();
        let GraphClaims {
            nodes: _,
            files,
            targets,
            with_own_files,
            set_targets,
            package_dirs,
            build_files,
            bzl_files,
            notify_sets,
            incomplete,
        } = claims;

        let mut by_label: BTreeMap<u64, (u32, u32)> = BTreeMap::new();
        for (label, set) in &targets {
            let entry = by_label
                .entry(stable_hash(&label.unconfigured().to_string()))
                .or_insert((NO_SET, NO_SET));
            entry.0 = entry.0.min(*set as u32);
        }
        for (label, set) in &with_own_files {
            let entry = by_label
                .entry(stable_hash(&label.unconfigured().to_string()))
                .or_insert((NO_SET, NO_SET));
            entry.1 = entry.1.min(*set as u32);
        }

        let mut other_files: Vec<(u64, u32)> = files
            .iter()
            .map(|(path, (set, _))| (stable_hash(path.as_str()), *set as u32))
            .collect();
        other_files.sort_unstable();
        // The lowest set of a hash comes first, and is the one found.
        other_files.dedup_by_key(|(hash, _)| *hash);

        // The contents of the files that say what the graph is.
        let mut graph_paths: Vec<CellPath> =
            build_files.values().map(|(_, path)| path.dupe()).collect();
        graph_paths.extend(bzl_files.values().map(|path| path.dupe()));
        let digests: BuckMutMap<CellPath, Option<String>> = dice
            .try_compute_join(graph_paths, async |dice, path| {
                buck2_error::Ok((path.dupe(), file_digest(dice, path).await?))
            })
            .await?
            .into_iter()
            .collect();
        let digest_of = |path: &CellPath| digests.get(path).cloned().flatten();
        let bzl_digest = files_digest(
            &mut bzl_files
                .iter()
                .map(|(path, cell_path)| (path, digest_of(cell_path)))
                .collect(),
        );
        let mut graph_files: Vec<(&ProjectRelativePathBuf, Option<String>)> = build_files
            .iter()
            .map(|(path, (_, cell_path))| (path, digest_of(cell_path)))
            .collect();
        graph_files.extend(
            bzl_files
                .iter()
                .map(|(path, cell_path)| (path, digest_of(cell_path))),
        );
        let graph_digest = files_digest(&mut graph_files);
        timer.step("the contents of the build files and `.bzl` files");

        let mut build_file_hashes: Vec<(u64, bool)> = build_files
            .iter()
            .map(|(path, (waits, _))| (stable_hash(path.as_str()), *waits))
            .collect();
        build_file_hashes.sort_unstable();
        let mut bzl_hashes: Vec<u64> = bzl_files
            .keys()
            .map(|path| stable_hash(path.as_str()))
            .collect();
        bzl_hashes.sort_unstable();

        let targets_digests: Vec<String> = set_targets
            .iter()
            .map(|nodes| targets_digest(nodes.iter().collect()))
            .collect();
        timer.step("the digests of the targets of each set");
        let interest = Arc::new(FileInterest::new(
            package_dirs.iter().map(|dir| &**dir),
            other_files
                .iter()
                .map(|(hash, _)| *hash)
                .chain(build_file_hashes.iter().map(|(hash, _)| *hash))
                .chain(bzl_hashes.iter().copied()),
        ));
        Ok(DevClaims {
            targets: by_label
                .into_iter()
                .map(|(hash, (set, own))| (hash, set, own))
                .collect(),
            files: other_files,
            interest,
            build_files: build_file_hashes,
            bzl_files: bzl_hashes,
            targets_digests,
            target_counts: set_targets.iter().map(|nodes| nodes.len()).collect(),
            bzl_digest,
            graph_digest,
            notify_sets,
            incomplete,
        })
    }

    fn state(&self) -> DevWatchState {
        DevWatchState {
            sets: self
                .targets_digests
                .iter()
                .map(|targets_digest| DevSetState {
                    targets_digest: targets_digest.clone(),
                })
                .collect(),
            bzl_digest: self.bzl_digest.clone(),
            graph_digest: self.graph_digest.clone(),
        }
    }

    /// Whether `path` may be something to the watch sets. This looks at nothing but the path, so
    /// it is cheap enough for every change in the project: `false` means that `classify` would
    /// say `Nothing`.
    fn may_claim(&self, path: &ProjectRelativePath) -> bool {
        self.interest.may_claim(path)
    }

    /// What `path` is to the watch sets, with the files as they are now.
    async fn classify(
        &self,
        dice: &mut DiceComputations<'_>,
        cell_resolver: &CellResolver,
        path: &ProjectRelativePath,
    ) -> buck2_error::Result<Claimed> {
        let hash = stable_hash(path.as_str());
        // A build file changes the graph, even when a target also uses it as a source file.
        if let Ok(i) = self.build_files.binary_search_by_key(&hash, |x| x.0) {
            return Ok(Claimed::BuildFile {
                waits: self.build_files[i].1,
            });
        }
        if self.bzl_files.binary_search(&hash).is_ok() {
            return Ok(Claimed::Bzl);
        }
        let mut set = match self.files.binary_search_by_key(&hash, |x| x.0) {
            Ok(i) => self.files[i].1,
            Err(_) => NO_SET,
        };
        // The targets that have the file as a source file are in the packages that the file is
        // in. Only the packages of the graph are looked at, and those are loaded already.
        let cell_path = cell_resolver.get_cell_path(&path);
        let mut dir = path.parent();
        while let Some(parent) = dir {
            dir = parent.parent();
            if !self.interest.is_package_dir(parent) {
                continue;
            }
            let package = PackageLabel::from_cell_path(cell_resolver.get_cell_path(&parent))?;
            let targets = dice.get_interpreter_results(package).await?;
            for node in targets.targets().values() {
                let Ok(i) = self
                    .targets
                    .binary_search_by_key(&stable_hash(&node.label().to_string()), |x| x.0)
                else {
                    continue;
                };
                let own = self.targets[i].2;
                if own < set && node.inputs().any(|input| input == cell_path) {
                    set = own;
                }
            }
        }
        Ok(if set == NO_SET {
            Claimed::Nothing
        } else {
            Claimed::Source(set as usize)
        })
    }
}

/// The `DevClaims` of a target.
#[derive(Clone, Dupe, Debug, Eq, Hash, PartialEq, Allocative, Pagable)]
#[pagable_typetag(dice::DiceKeyDyn)]
struct DevClaimsKey(ConfiguredProvidersLabel);

impl std::fmt::Display for DevClaimsKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DevClaims({})", self.0)
    }
}

#[async_trait]
impl Key for DevClaimsKey {
    type Value = buck2_error::Result<Arc<DevClaims>>;

    async fn compute(
        &self,
        ctx: &mut DiceComputations,
        _cancellations: &CancellationContext,
    ) -> Self::Value {
        let watch = dev_watch(ctx, &self.0).await?;
        let claims = graph_claims(ctx, &self.0, &watch).await?;
        Ok(Arc::new(DevClaims::new(ctx, claims).await?))
    }

    fn equality_behavior() -> EqualityBehavior<Self::Value> {
        EqualityBehavior::Compare(|x, y| match (x, y) {
            (Ok(x), Ok(y)) => x == y,
            _ => false,
        })
    }

    fn validity(x: &Self::Value) -> bool {
        x.is_ok()
    }

    fn value_serialize() -> impl ValueSerialize<Value = Self::Value> {
        NoValueSerialize::<Self::Value>::new()
    }
}

async fn dev_claims(
    dice: &mut DiceComputations<'_>,
    label: &ConfiguredProvidersLabel,
) -> buck2_error::Result<Arc<DevClaims>> {
    dice.compute(&DevClaimsKey(label.dupe())).await?.dupe()
}

/// The `DevInfo.watch` of a target.
async fn dev_watch(
    dice: &mut DiceComputations<'_>,
    label: &ConfiguredProvidersLabel,
) -> buck2_error::Result<Vec<DevWatchSpec>> {
    let providers = dice.get_providers(label).await?.require_compatible()?;
    let info = providers
        .builtin_provider_value::<DevInfo>()?
        .ok_or_else(|| DevError::NoDevInfo(label.dupe()))?;
    info.as_ref().value().as_ref().watch()
}

/// Say what the requested paths are to the watch sets, and which sets' targets changed since
/// `request.previous`. Whether a file changed is not said here: a path that `buck2 dev` asks
/// about did, unless its digest is the one that `buck2 dev` knows.
async fn watch_files(
    dice: &mut DiceComputations<'_>,
    label: &ConfiguredProvidersLabel,
    watch: &[DevWatchSpec],
    request: &DevWatchRequest,
    session: &str,
) -> buck2_error::Result<DevWatched> {
    let claims = dev_claims(dice, label).await?;
    // From here on the session's file change subscription drops the changes that cannot matter
    // to these claims. `buck2 dev` asks again after every change that is let through, and so
    // after every change to the claims.
    set_interest(session, claims.interest.dupe());
    let cell_resolver = dice.get_cell_resolver().await?;

    let mut paths = Vec::with_capacity(request.paths.len());
    for path in &request.paths {
        let Ok(rel) = ProjectRelativePath::new(path) else {
            continue;
        };
        let (set, waits) = if claims.may_claim(rel) {
            match claims.classify(dice, cell_resolver, rel).await? {
                Claimed::Source(set) => (Some(set), watch[set].notify),
                Claimed::BuildFile { waits } => (None, waits),
                Claimed::Bzl => (None, claims.notify_sets),
                Claimed::Nothing => (None, false),
            }
        } else {
            (None, false)
        };
        // Only for the files whose contents say whether they changed.
        let digest = if set.is_some() && !waits {
            file_digest(dice, cell_resolver.get_cell_path(&rel)).await?
        } else {
            None
        };
        paths.push(DevPath {
            path: path.clone(),
            set,
            waits,
            digest,
        });
    }

    // Only the sets that restart are compared by the contents of their files from the start: a
    // restart that nothing called for stops a running program, whereas a build that nothing
    // called for builds nothing. The build files and `.bzl` files whose change waits are here
    // too, without their contents, for when the analysis fails and cannot say what a path is.
    let mut compared = Vec::new();
    if request.all_compared && watch.iter().any(|spec| spec.restart || spec.notify) {
        let graph = graph_claims(dice, label, watch).await?;
        // The files of the sets that restart: those they claim by name, and the source files of
        // the targets whose own source files they claim.
        let mut restarts: BTreeMap<ProjectRelativePathBuf, (usize, CellPath)> = BTreeMap::new();
        for (path, (set, cell_path)) in &graph.files {
            if watch[*set].restart {
                restarts.insert(path.clone(), (*set, cell_path.dupe()));
            }
        }
        for node in &graph.nodes {
            let Some(own) = graph.with_own_files.get(node.label()) else {
                continue;
            };
            for input in node.inputs() {
                let set = graph.set_of_own_file(cell_resolver, input, *own);
                if !watch[set].restart {
                    continue;
                }
                if let Some(path) = project_path(cell_resolver, input)? {
                    if !graph.build_files.contains_key(&path) {
                        restarts.entry(path).or_insert((set, input));
                    }
                }
            }
        }
        compared = dice
            .try_compute_join(restarts, async |dice, (path, (set, cell_path))| {
                buck2_error::Ok(DevPath {
                    path: path.to_string(),
                    set: Some(set),
                    waits: false,
                    digest: file_digest(dice, cell_path).await?,
                })
            })
            .await?;
        let waiting = graph
            .build_files
            .iter()
            .filter(|(_, (waits, _))| *waits)
            .map(|(path, _)| path)
            .chain(graph.bzl_files.keys().filter(|_| graph.notify_sets));
        compared.extend(waiting.map(|path| DevPath {
            path: path.to_string(),
            set: None,
            waits: true,
            digest: None,
        }));
    }

    let state = claims.state();
    let (changed_sets, graph_changed) = match &request.previous {
        None => (Vec::new(), false),
        Some(previous) if previous.sets.len() != state.sets.len() => {
            ((0..state.sets.len()).collect(), true)
        }
        Some(previous) => {
            // A `.bzl` file may change what any rule does, so it changes every set.
            let every = previous.bzl_digest != state.bzl_digest;
            let changed: Vec<usize> = previous
                .sets
                .iter()
                .zip(&state.sets)
                .enumerate()
                .filter(|(_, (old, new))| every || old != new)
                .map(|(set, _)| set)
                .collect();
            let targets_changed = previous.sets != state.sets;
            (
                changed,
                targets_changed || previous.graph_digest != state.graph_digest,
            )
        }
    };

    Ok(DevWatched {
        watch: watch
            .iter()
            .zip(&claims.target_counts)
            .map(|(spec, targets)| DevWatchSet {
                build: spec.build.clone(),
                restart: spec.restart,
                notify: spec.notify,
                targets: *targets,
            })
            .collect(),
        state,
        paths,
        compared,
        changed_sets,
        graph_changed,
        incomplete: claims.incomplete.clone(),
    })
}

/// The contents of a file, as the daemon's file watcher keeps them up to date, or `None` if it
/// does not exist.
async fn file_digest(
    dice: &mut DiceComputations<'_>,
    path: CellPath,
) -> buck2_error::Result<Option<String>> {
    Ok(
        match DiceFileComputations::read_path_metadata_if_exists(dice, path).await? {
            None => None,
            Some(RawPathMetadata::File(file)) => Some(format!(
                "{}{}",
                file.digest.data(),
                if file.is_executable { "+x" } else { "" }
            )),
            Some(RawPathMetadata::Symlink { to, .. }) => Some(format!("-> {to:?}")),
            Some(RawPathMetadata::Directory) => Some("directory".to_owned()),
        },
    )
}

/// A digest of a set of files and their contents, which is the same in another daemon.
fn files_digest(files: &mut Vec<(&ProjectRelativePathBuf, Option<String>)>) -> String {
    files.sort();
    let mut hasher = DefaultHasher::new();
    files.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// How many symlinks in a row `claims` follows from a source file.
const MAX_SYMLINK_HOPS: usize = 8;

fn claim<K: Hash + Eq>(map: &mut BuckMutMap<K, usize>, key: K, set: usize) {
    let entry = map.entry(key).or_insert(set);
    *entry = (*entry).min(set);
}

fn claim_file(
    files: &mut BuckMutMap<ProjectRelativePathBuf, (usize, CellPath)>,
    cell_resolver: &CellResolver,
    path: CellPath,
    set: usize,
) -> buck2_error::Result<Option<ProjectRelativePathBuf>> {
    let Some(rel) = project_path(cell_resolver, path.dupe())? else {
        return Ok(None);
    };
    let entry = files.entry(rel.clone()).or_insert((set, path));
    entry.0 = entry.0.min(set);
    Ok(Some(rel))
}

/// A digest of the attributes of `nodes`, which is the same in another daemon.
fn targets_digest(mut nodes: Vec<&ConfiguredTargetNode>) -> String {
    nodes.sort_by_key(|node| node.label());
    let mut hasher = DefaultHasher::new();
    hasher.write_usize(nodes.len());
    for node in nodes {
        node.target_hash_without_configured_labels(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

/// What the `inputs` of a watch set lead to, in the actions of the targets that own them.
#[derive(Default)]
struct InputsWalk {
    /// The source files that those actions use.
    sources: Vec<CellPath>,
    /// The targets that own the inputs.
    owners: BuckMutSet<ConfiguredTargetLabel>,
    /// The other targets that own what those actions use.
    beyond: BuckMutSet<ConfiguredTargetLabel>,
    /// Inputs that could not be expanded, with the target that owns them.
    failed: Vec<(Option<ConfiguredTargetLabel>, buck2_error::Error)>,
}

/// A step of `walk_inputs`.
enum WalkStep {
    /// The inputs of an action.
    Action(ActionKey),
    /// The artifacts and children of a transitive set projection.
    Projection(TransitiveSetProjectionKey),
    /// The artifact that a promise resolves to.
    Promise(ArtifactGroup),
}

impl WalkStep {
    fn owner(&self) -> Option<ConfiguredTargetLabel> {
        let owner = match self {
            WalkStep::Action(key) => key.owner(),
            WalkStep::Projection(key) => key.key.holder_key().owner(),
            WalkStep::Promise(_) => return None,
        };
        match owner {
            BaseDeferredKey::TargetLabel(label) => Some(label.dupe()),
            BaseDeferredKey::AnonTarget(_) | BaseDeferredKey::BxlLabel(_) => None,
        }
    }

    async fn expand(
        &self,
        dice: &mut DiceComputations<'_>,
    ) -> buck2_error::Result<Vec<ArtifactGroup>> {
        match self {
            // An action that a dynamic action makes is only known once the dynamic action has
            // run, so this builds what the dynamic action needs.
            WalkStep::Action(key) => Ok(ActionCalculation::get_action(dice, key)
                .await?
                .inputs()?
                .into_owned()),
            WalkStep::Projection(key) => {
                let set = key.key.lookup(dice).await?;
                set.by_ref(|s| s.get_projection_sub_inputs(key.projection))
            }
            WalkStep::Promise(group) => Ok(match group.resolved_artifact(dice).await? {
                ResolvedArtifactGroup::Artifact(artifact) => {
                    vec![ArtifactGroup::Artifact(artifact.dupe())]
                }
                ResolvedArtifactGroup::TransitiveSetProjection(_) => Vec::new(),
            }),
        }
    }
}

/// Find the source files that the actions of the targets that own `inputs` use, and the other
/// targets that own what those actions use. The walk stops there: what it would find in the
/// action graph below is built from the source files of those targets and their dependencies,
/// which is all that the caller needs to know, and the action graph of a large target takes
/// far longer to walk than its targets.
///
/// The walk goes one level of the graph at a time, so that each level is computed concurrently.
async fn walk_inputs(dice: &mut DiceComputations<'_>, inputs: &[ArtifactGroup]) -> InputsWalk {
    let mut walk = InputsWalk::default();
    let mut seen_sources: BuckMutSet<CellPath> = BuckMutSet::default();
    let mut seen_actions: BuckMutSet<ActionKey> = BuckMutSet::default();
    let mut seen_projections: BuckMutSet<TransitiveSetProjectionKey> = BuckMutSet::default();
    // Each with whether it is one of `inputs`, whose owner the walk stays in.
    let mut frontier: Vec<(ArtifactGroup, bool)> =
        inputs.iter().map(|input| (input.dupe(), true)).collect();
    while !frontier.is_empty() {
        let mut steps = Vec::new();
        for (input, is_input) in std::mem::take(&mut frontier) {
            let step = match &input {
                ArtifactGroup::Artifact(artifact) => {
                    if let Some(source) = artifact.get_source() {
                        let path = source.get_path().to_cell_path();
                        if seen_sources.insert(path.clone()) {
                            walk.sources.push(path);
                        }
                        continue;
                    }
                    match artifact.action_key() {
                        Some(key) => WalkStep::Action(key.dupe()),
                        None => continue,
                    }
                }
                ArtifactGroup::TransitiveSetProjection(projection) => {
                    WalkStep::Projection(projection.key.dupe())
                }
                ArtifactGroup::Promise(_) => WalkStep::Promise(input.dupe()),
            };
            // What a target does not own, such as an anonymous target, is walked like the
            // owners of the inputs: it has no source files of its own to fall back on.
            if let Some(owner) = step.owner() {
                if is_input {
                    walk.owners.insert(owner);
                } else if !walk.owners.contains(&owner) {
                    walk.beyond.insert(owner);
                    continue;
                }
            }
            let new = match &step {
                WalkStep::Action(key) => seen_actions.insert(key.dupe()),
                WalkStep::Projection(key) => seen_projections.insert(key.dupe()),
                WalkStep::Promise(_) => true,
            };
            if new {
                steps.push((step, is_input));
            }
        }
        let expanded = dice
            .compute_join(steps, async |dice, (step, is_input)| {
                // What a promise resolves to is an input if the promise was.
                let is_input = is_input && matches!(step, WalkStep::Promise(_));
                (step.owner(), is_input, step.expand(dice).await)
            })
            .await;
        for (owner, is_input, result) in expanded {
            match result {
                Ok(inputs) => frontier.extend(inputs.into_iter().map(|input| (input, is_input))),
                Err(e) => walk.failed.push((owner, e)),
            }
        }
    }
    walk
}

/// The targets `roots` and their dependencies.
fn dependency_closure(
    roots: impl IntoIterator<Item = ConfiguredTargetNode>,
) -> Vec<ConfiguredTargetNode> {
    let mut seen = BuckMutSet::default();
    let mut queue = VecDeque::new();
    let mut nodes = Vec::new();
    for root in roots {
        if seen.insert(root.label().dupe()) {
            queue.push_back(root);
        }
    }
    while let Some(node) = queue.pop_front() {
        for dep in node.deps() {
            if seen.insert(dep.label().dupe()) {
                queue.push_back(dep.dupe());
            }
        }
        nodes.push(node);
    }
    nodes
}

/// The project-relative path of a source path, or `None` for a path under `buck-out`, such as a
/// file of a bundled external cell, which cannot be edited.
fn project_path(
    cell_resolver: &CellResolver,
    path: CellPath,
) -> buck2_error::Result<Option<ProjectRelativePathBuf>> {
    let rel = cell_resolver.resolve_path(path)?;
    if is_buck_out(&rel) {
        return Ok(None);
    }
    Ok(Some(rel))
}

fn is_buck_out(path: &ProjectRelativePath) -> bool {
    path.as_str() == "buck-out" || path.as_str().starts_with("buck-out/")
}

/// The `.bzl` files that the build files of `packages` load, directly or through other `.bzl`
/// files.
async fn loaded_files(
    dice: &mut DiceComputations<'_>,
    cell_resolver: &CellResolver,
    packages: BTreeSet<buck2_core::package::PackageLabel>,
) -> buck2_error::Result<BTreeMap<ProjectRelativePathBuf, CellPath>> {
    let mut seen: BuckMutSet<ImportPath> = BuckMutSet::default();
    let mut queue: VecDeque<ImportPath> = VecDeque::new();
    for package in packages {
        let result = dice.get_interpreter_results(package).await?;
        for import in result.imports() {
            if seen.insert(import.clone()) {
                queue.push_back(import.clone());
            }
        }
    }
    while let Some(import) = queue.pop_front() {
        for next in dice.get_loaded_module_imports(&import).await? {
            if seen.insert(next.clone()) {
                queue.push_back(next);
            }
        }
    }
    let mut files = BTreeMap::new();
    for import in seen {
        let cell_path = import.path().to_owned();
        if let Some(path) = project_path(cell_resolver, cell_path.dupe())? {
            files.insert(path, cell_path);
        }
    }
    Ok(files)
}
