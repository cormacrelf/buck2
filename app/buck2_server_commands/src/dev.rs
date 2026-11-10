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
//! - `Watch` expands the watch policy into files. Expanding `DevWatch.inputs` walks the action
//!   graph, which builds what dynamic actions need to say what they do.
//! - `Build` builds named `DevInfo` outputs.
//!
//! The client runs the server, and acts on the file changes that the daemon's file watcher pushes
//! to it through a subscription, so no daemon command lasts longer than one build.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::VecDeque;
use std::hash::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::sync::Arc;
use std::sync::Mutex;

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
use buck2_common::file_ops::metadata::RawPathMetadata;
use buck2_common::file_ops::metadata::RawSymlink;
use buck2_common::io::IoProvider;
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
use buck2_core::pattern::pattern::ParsedPattern;
use buck2_core::pattern::pattern_type::ProvidersPatternExtra;
use buck2_core::pattern::pattern_type::TargetPatternExtra;
use buck2_core::provider::label::ConfiguredProvidersLabel;
use buck2_core::target::configured_target_label::ConfiguredTargetLabel;
use buck2_data::BuildResult;
use buck2_error::BuckErrorContext;
use buck2_error::internal_error;
use buck2_execute::artifact::fs::ExecutorFs;
use buck2_hash::BuckMutMap;
use buck2_hash::BuckMutSet;
use buck2_interpreter::load_module::InterpreterCalculation;
use buck2_node::nodes::configured::ConfiguredTargetNode;
use buck2_node::nodes::configured_frontend::ConfiguredTargetNodeCalculation;
use buck2_node::nodes::frontend::TargetGraphCalculation;
use buck2_node::target_calculation::ConfiguredTargetCalculation;
use buck2_server_ctx::ctx::FileChangeBatch;
use buck2_server_ctx::ctx::ServerCommandContextTrait;
use buck2_server_ctx::global_cfg_options::global_cfg_options_from_client_context;
use buck2_server_ctx::partial_result_dispatcher::NoPartialResult;
use buck2_server_ctx::partial_result_dispatcher::PartialResultDispatcher;
use buck2_server_ctx::template::ServerCommandTemplate;
use buck2_server_ctx::template::run_server_command;
use dice::DiceComputations;
use dice::DiceTransaction;
use dupe::Dupe;
use futures::StreamExt;
use futures::stream::BoxStream;

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
        dev(server_ctx, &mut ctx, &self.req, changes).await
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
                watch_files(&mut ctx.ctx(), &label, &watch, request).await?,
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
            // The files that the build must not outlive a change to, as this command sees them.
            let watch = info.as_ref().value().as_ref().watch()?;
            let files = claims(&mut ctx.ctx(), &label, &watch)
                .await?
                .without_waiting(&watch);
            let known: BuckMutMap<ProjectRelativePathBuf, Option<FileShape>> = ctx
                .ctx()
                .try_compute_join(files, async |dice, (path, cell_path)| {
                    let metadata =
                        DiceFileComputations::read_path_metadata_if_exists(dice, cell_path).await?;
                    buck2_error::Ok((path, metadata.map(FileShape::of)))
                })
                .await?
                .into_iter()
                .collect();
            let io = ctx
                .ctx()
                .global_data()
                .get::<Arc<dyn IoProvider>>()
                .map_err(|_| internal_error!("The daemon has no I/O provider"))?
                .dupe();
            let mut dice = ctx.ctx();
            tokio::select! {
                built = build(server_ctx.project_root(), &mut dice, wanted) => {
                    Ok(DevResponse::Build(built?))
                }
                // Dropping the build cancels it.
                interrupted = changed_files(changes, known, io) => {
                    Ok(DevResponse::Interrupted(interrupted))
                }
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

/// Wait for one of the `known` files to change from what this command saw, and say which did.
/// A file saved without a change, or one that nothing builds from, is no change.
async fn changed_files(
    mut changes: BoxStream<'static, FileChangeBatch>,
    known: BuckMutMap<ProjectRelativePathBuf, Option<FileShape>>,
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
            let Some(before) = known.get(&path) else {
                continue;
            };
            let now = io
                .read_path_metadata_if_exists_impl(IProjectRelativePath::intern_directly(&path))
                .await
                .map(|metadata| metadata.map(FileShape::of));
            // A file that cannot be read now counts as changed.
            if now.as_ref().ok() != Some(before) {
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

/// What the watch sets claim.
struct Claims {
    /// Each source file, with the set that claims it.
    files: BuckMutMap<ProjectRelativePathBuf, (usize, CellPath)>,
    /// The targets of each set.
    set_targets: Vec<Vec<ConfiguredTargetNode>>,
    /// Each build file of the graph, with whether a change to it waits.
    build_files: BTreeMap<ProjectRelativePathBuf, (bool, CellPath)>,
    /// The `.bzl` files that the graph loads.
    bzl_files: BTreeMap<ProjectRelativePathBuf, CellPath>,
    /// Whether there is a `notify` set. A change to a `.bzl` file then waits.
    notify_sets: bool,
    incomplete: Vec<String>,
}

impl Claims {
    /// The files whose change makes a build out of date: those that a set that builds or
    /// restarts claims, and the build files and `.bzl` files whose change does not wait.
    fn without_waiting(self, watch: &[DevWatchSpec]) -> Vec<(ProjectRelativePathBuf, CellPath)> {
        let mut files: Vec<(ProjectRelativePathBuf, CellPath)> = self
            .files
            .into_iter()
            .filter(|(_, (set, _))| !watch[*set].build.is_empty() || watch[*set].restart)
            .map(|(path, (_, cell_path))| (path, cell_path))
            .collect();
        files.extend(
            self.build_files
                .into_iter()
                .filter(|(_, (waits, _))| !waits)
                .map(|(path, (_, cell_path))| (path, cell_path)),
        );
        if !self.notify_sets {
            files.extend(self.bzl_files);
        }
        files
    }
}

/// Walk the configured dependency graph of `label`, and the action graph below the `inputs` of
/// each watch set. Give each source file, and each target, to the first watch set that claims
/// it.
async fn claims(
    dice: &mut DiceComputations<'_>,
    label: &ConfiguredProvidersLabel,
    watch: &[DevWatchSpec],
) -> buck2_error::Result<Claims> {
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
    let nodes = dependency_closure(root);
    let by_label: BuckMutMap<ConfiguredTargetLabel, ConfiguredTargetNode> = nodes
        .iter()
        .map(|node| (node.label().dupe(), node.dupe()))
        .collect();

    // The lowest watch set index that claims each source file, and each target. A set claims
    // the targets that its `targets` match, with their own source files. It also claims the
    // targets that own the actions below its `inputs`, but not their other source files: only
    // the ones those actions use. The targets of a set say which sets a build file change
    // changed.
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
        for (owner, error) in walk.failed {
            incomplete.push(format!("`DevInfo.watch[{set}].inputs`: {error:#}"));
            // The input comes from the owner's analysis, so it can only be built from the
            // source files of the owner and its dependencies.
            if let Some(node) = owner.and_then(|owner| by_label.get(&owner)) {
                for node in dependency_closure(node) {
                    claim(&mut targets, node.label().dupe(), set);
                    claim(&mut with_own_files, node.label().dupe(), set);
                }
            }
        }
    }

    let notify_sets = watch.iter().any(|spec| spec.notify);
    let mut set_targets: Vec<Vec<ConfiguredTargetNode>> = vec![Vec::new(); watch.len()];
    // Each build file, with whether a change to it waits.
    let mut build_files: BTreeMap<ProjectRelativePathBuf, (bool, CellPath)> = BTreeMap::new();
    let mut packages = BTreeSet::new();
    for node in &nodes {
        if let Some(&set) = with_own_files.get(node.label()) {
            for input in node.inputs() {
                claim_file(&mut files, cell_resolver, input, set)?;
            }
        }
        let set = targets.get(node.label()).copied();
        if let Some(set) = set {
            set_targets[set].push(node.dupe());
        }
        packages.insert(node.label().pkg());
        let buildfile = node.buildfile_path().path();
        if let Some(path) = project_path(cell_resolver, buildfile.dupe())? {
            build_files.entry(path).or_insert((false, buildfile)).0 |=
                set.is_some_and(|set| watch[set].notify);
        }
    }
    // A source file that is a symlink is built from the file that it points at, which the set
    // claims too.
    let mut links: Vec<(usize, CellPath)> = files
        .values()
        .map(|(set, path)| (*set, path.dupe()))
        .collect();
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
            let new = project_path(cell_resolver, target.dupe())?
                .is_some_and(|rel| !files.contains_key(&rel));
            claim_file(&mut files, cell_resolver, target.dupe(), set)?;
            if new {
                links.push((set, target));
            }
        }
    }
    let bzl_files = loaded_files(dice, cell_resolver, packages).await?;
    // A build file changes the graph, even when a target also uses it as a source file.
    for path in build_files.keys() {
        files.remove(path);
    }
    Ok(Claims {
        files,
        set_targets,
        build_files,
        bzl_files,
        notify_sets,
        incomplete,
    })
}

/// Find what the watch sets claim. Then say what the requested paths are, and what changed since
/// `request.previous`, from the files as they are now.
async fn watch_files(
    dice: &mut DiceComputations<'_>,
    label: &ConfiguredProvidersLabel,
    watch: &[DevWatchSpec],
    request: &DevWatchRequest,
) -> buck2_error::Result<DevWatched> {
    let Claims {
        files,
        set_targets,
        build_files,
        bzl_files,
        notify_sets,
        incomplete,
    } = claims(dice, label, watch).await?;
    let cell_resolver = dice.get_cell_resolver().await?;

    // The contents of every file that the answer depends on, as the daemon knows them.
    let mut cell_paths: Vec<CellPath> = files.values().map(|(_, path)| path.dupe()).collect();
    cell_paths.extend(build_files.values().map(|(_, path)| path.dupe()));
    cell_paths.extend(bzl_files.values().map(|path| path.dupe()));
    let digests: BuckMutMap<CellPath, Option<String>> = dice
        .try_compute_join(cell_paths, async |dice, path| {
            buck2_error::Ok((path.dupe(), file_digest(dice, path).await?))
        })
        .await?
        .into_iter()
        .collect();
    let digest_of = |path: &CellPath| digests.get(path).cloned().flatten();

    let mut set_files: Vec<Vec<(&ProjectRelativePathBuf, Option<String>)>> =
        vec![Vec::new(); watch.len()];
    for (path, (set, cell_path)) in &files {
        set_files[*set].push((path, digest_of(cell_path)));
    }
    let sets: Vec<DevSetState> = set_targets
        .into_iter()
        .zip(&mut set_files)
        .map(|(nodes, files)| DevSetState {
            targets_digest: targets_digest(nodes.iter().collect()),
            files_digest: files_digest(files),
        })
        .collect();
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
    let state = DevWatchState {
        sets,
        bzl_digest,
        graph_digest: files_digest(&mut graph_files),
    };

    // A change to a file of a `notify` set waits, and so does a change to what may change its
    // targets: the build file of a package with one of them, and with any `notify` set, any
    // `.bzl` file.
    let waits = |path: &ProjectRelativePath| match files.get(path) {
        Some((set, _)) => watch[*set].notify,
        None => {
            build_files.get(path).is_some_and(|(waits, _)| *waits)
                || (notify_sets && bzl_files.contains_key(path))
        }
    };
    let mut paths = Vec::with_capacity(request.paths.len());
    for path in &request.paths {
        let Ok(rel) = ProjectRelativePath::new(path) else {
            continue;
        };
        let set = files.get(rel).map(|(set, _)| *set);
        let waits = waits(rel);
        // Only for files that something uses: the others need no digest.
        let digest = if set.is_some() || waits {
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
    let mut waiting = Vec::new();
    if request.waiting_digests && notify_sets {
        for (path, (set, cell_path)) in &files {
            if watch[*set].notify {
                waiting.push(dev_path(path, Some(*set), digest_of(cell_path)));
            }
        }
        for (path, (waits, cell_path)) in &build_files {
            if *waits {
                waiting.push(dev_path(path, None, digest_of(cell_path)));
            }
        }
        for (path, cell_path) in &bzl_files {
            waiting.push(dev_path(path, None, digest_of(cell_path)));
        }
    }

    let (changed_sets, graph_changed) = match &request.previous {
        None => (Vec::new(), false),
        Some(previous) if previous.sets.len() != state.sets.len() => {
            ((0..state.sets.len()).collect(), true)
        }
        Some(previous) => {
            // A `.bzl` file may change what any rule does, so it changes every set.
            let every = previous.bzl_digest != state.bzl_digest;
            let changed = previous
                .sets
                .iter()
                .zip(&state.sets)
                .enumerate()
                .filter(|(_, (old, new))| every || old != new)
                .map(|(set, _)| set)
                .collect();
            let targets_changed = previous
                .sets
                .iter()
                .zip(&state.sets)
                .any(|(old, new)| old.targets_digest != new.targets_digest);
            (
                changed,
                targets_changed || previous.graph_digest != state.graph_digest,
            )
        }
    };

    let mut files_per_set = vec![0; watch.len()];
    for (set, _) in files.values() {
        files_per_set[*set] += 1;
    }
    Ok(DevWatched {
        watch: watch
            .iter()
            .zip(files_per_set)
            .map(|(spec, files)| DevWatchSet {
                build: spec.build.clone(),
                restart: spec.restart,
                notify: spec.notify,
                files,
            })
            .collect(),
        state,
        paths,
        waiting,
        changed_sets,
        graph_changed,
        incomplete,
    })
}

fn dev_path(path: &ProjectRelativePath, set: Option<usize>, digest: Option<String>) -> DevPath {
    DevPath {
        path: path.to_string(),
        set,
        waits: true,
        digest,
    }
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
) -> buck2_error::Result<()> {
    if let Some(rel) = project_path(cell_resolver, path.dupe())? {
        let entry = files.entry(rel).or_insert((set, path));
        entry.0 = entry.0.min(set);
    }
    Ok(())
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

/// What the `inputs` of a watch set lead to in the action graph.
#[derive(Default)]
struct InputsWalk {
    /// The source files that the inputs are built from.
    sources: Vec<CellPath>,
    /// The targets that own the actions and transitive sets on the way.
    owners: BuckMutSet<ConfiguredTargetLabel>,
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

/// Find the source files that `inputs` are built from. The walk goes one level of the graph at a
/// time, so that each level is computed concurrently, and so that deep transitive sets do not
/// need deep recursion.
async fn walk_inputs(dice: &mut DiceComputations<'_>, inputs: &[ArtifactGroup]) -> InputsWalk {
    let mut walk = InputsWalk::default();
    let mut seen_sources: BuckMutSet<CellPath> = BuckMutSet::default();
    let mut seen_actions: BuckMutSet<ActionKey> = BuckMutSet::default();
    let mut seen_projections: BuckMutSet<TransitiveSetProjectionKey> = BuckMutSet::default();
    let mut frontier: Vec<ArtifactGroup> = inputs.to_vec();
    while !frontier.is_empty() {
        let mut steps = Vec::new();
        for input in std::mem::take(&mut frontier) {
            match &input {
                ArtifactGroup::Artifact(artifact) => {
                    if let Some(source) = artifact.get_source() {
                        let path = source.get_path().to_cell_path();
                        if seen_sources.insert(path.clone()) {
                            walk.sources.push(path);
                        }
                    } else if let Some(key) = artifact.action_key() {
                        if seen_actions.insert(key.dupe()) {
                            steps.push(WalkStep::Action(key.dupe()));
                        }
                    }
                }
                ArtifactGroup::TransitiveSetProjection(projection) => {
                    if seen_projections.insert(projection.key.dupe()) {
                        steps.push(WalkStep::Projection(projection.key.dupe()));
                    }
                }
                ArtifactGroup::Promise(_) => steps.push(WalkStep::Promise(input.dupe())),
            }
        }
        let expanded = dice
            .compute_join(steps, async |dice, step| {
                (step.owner(), step.expand(dice).await)
            })
            .await;
        for (owner, result) in expanded {
            match result {
                Ok(inputs) => {
                    walk.owners.extend(owner);
                    frontier.extend(inputs);
                }
                Err(e) => walk.failed.push((owner, e)),
            }
        }
    }
    walk
}

fn dependency_closure(root: &ConfiguredTargetNode) -> Vec<ConfiguredTargetNode> {
    let mut seen = BuckMutSet::default();
    let mut queue = VecDeque::new();
    let mut nodes = Vec::new();
    seen.insert(root.label().dupe());
    queue.push_back(root.dupe());
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
