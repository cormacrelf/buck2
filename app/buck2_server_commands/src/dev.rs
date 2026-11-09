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
//! The client does the file watching and runs the server, so no daemon command lasts longer than
//! one build.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::VecDeque;
use std::hash::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;

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
use buck2_cli_proto::new_generic::DevBuildFile;
use buck2_cli_proto::new_generic::DevBuilt;
use buck2_cli_proto::new_generic::DevBuiltOutput;
use buck2_cli_proto::new_generic::DevRequest;
use buck2_cli_proto::new_generic::DevResponse;
use buck2_cli_proto::new_generic::DevServer;
use buck2_cli_proto::new_generic::DevWatchSet;
use buck2_cli_proto::new_generic::DevWatched;
use buck2_common::dice::cells::HasCellResolver;
use buck2_common::pattern::parse_from_cli::parse_patterns_from_cli_args;
use buck2_core::bzl::ImportPath;
use buck2_core::cells::CellResolver;
use buck2_core::cells::cell_path::CellPath;
use buck2_core::deferred::base_deferred_key::BaseDeferredKey;
use buck2_core::execution_types::executor_config::PathSeparatorKind;
use buck2_core::fs::project::ProjectRoot;
use buck2_core::fs::project_rel_path::ProjectRelativePath;
use buck2_core::pattern::pattern::ParsedPattern;
use buck2_core::pattern::pattern_type::ProvidersPatternExtra;
use buck2_core::pattern::pattern_type::TargetPatternExtra;
use buck2_core::provider::label::ConfiguredProvidersLabel;
use buck2_core::target::configured_target_label::ConfiguredTargetLabel;
use buck2_data::BuildResult;
use buck2_error::BuckErrorContext;
use buck2_execute::artifact::fs::ExecutorFs;
use buck2_hash::BuckMutMap;
use buck2_hash::BuckMutSet;
use buck2_interpreter::load_module::InterpreterCalculation;
use buck2_node::nodes::configured::ConfiguredTargetNode;
use buck2_node::nodes::configured_frontend::ConfiguredTargetNodeCalculation;
use buck2_node::nodes::frontend::TargetGraphCalculation;
use buck2_node::target_calculation::ConfiguredTargetCalculation;
use buck2_server_ctx::ctx::ServerCommandContextTrait;
use buck2_server_ctx::global_cfg_options::global_cfg_options_from_client_context;
use buck2_server_ctx::partial_result_dispatcher::NoPartialResult;
use buck2_server_ctx::partial_result_dispatcher::PartialResultDispatcher;
use buck2_server_ctx::template::ServerCommandTemplate;
use buck2_server_ctx::template::run_server_command;
use dice::DiceComputations;
use dice::DiceTransaction;
use dupe::Dupe;

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
    run_server_command(DevServerCommand { req }, ctx, partial_result_dispatcher).await
}

struct DevServerCommand {
    req: DevRequest,
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
        dev(server_ctx, &mut ctx, &self.req).await
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
            }))
        }
        DevAction::Watch => {
            let watch = info.as_ref().value().as_ref().watch()?;
            Ok(DevResponse::Watch(
                watch_files(server_ctx.project_root(), &mut ctx.ctx(), &label, &watch).await?,
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
            Ok(DevResponse::Build(
                build(server_ctx.project_root(), &mut ctx.ctx(), wanted).await?,
            ))
        }
    }
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

async fn build(
    project_root: &ProjectRoot,
    dice: &mut DiceComputations<'_>,
    wanted: Vec<(String, Artifact)>,
) -> buck2_error::Result<DevBuilt> {
    let artifact_fs = dice.get_artifact_fs().await?;
    let built = dice
        .try_compute_join(wanted, async |dice, (name, artifact)| {
            let values = materialize_and_upload_artifact_group(
                dice,
                &ArtifactGroup::Artifact(artifact.dupe()),
                MaterializationAndUploadContext::materialize(),
                &dice
                    .per_transaction_data()
                    .get_materialization_queue_tracker(),
            )
            .await?;
            buck2_error::Ok((name, values))
        })
        .await?;
    let mut outputs = BTreeMap::new();
    for (name, values) in built {
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
    Ok(DevBuilt { outputs })
}

/// Walk the configured dependency graph of `label`, and the action graph below the `inputs` of
/// each watch set. Give each source file, and each target, to the first watch set that claims
/// it.
async fn watch_files(
    project_root: &ProjectRoot,
    dice: &mut DiceComputations<'_>,
    label: &ConfiguredProvidersLabel,
    watch: &[DevWatchSpec],
) -> buck2_error::Result<DevWatched> {
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
    let mut files: BuckMutMap<String, usize> = BuckMutMap::default();
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
            if let Some(path) = abs_source_path(project_root, cell_resolver, path)? {
                claim(&mut files, path, set);
            }
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

    let mut set_targets: Vec<Vec<&ConfiguredTargetNode>> = vec![Vec::new(); watch.len()];
    let mut build_files: BTreeMap<String, bool> = BTreeMap::new();
    let mut dirs = BTreeSet::new();
    let mut packages = BTreeSet::new();
    for node in &nodes {
        if let Some(&set) = with_own_files.get(node.label()) {
            for input in node.inputs() {
                if let Some(path) = abs_source_path(project_root, cell_resolver, input)? {
                    claim(&mut files, path, set);
                }
            }
        }
        let set = targets.get(node.label()).copied();
        if let Some(set) = set {
            set_targets[set].push(node);
        }
        packages.insert(node.label().pkg());
        if let Some(path) =
            abs_source_path(project_root, cell_resolver, node.buildfile_path().path())?
        {
            *build_files.entry(path).or_default() |= set.is_some_and(|set| watch[set].notify);
        }
        if let Some(path) = abs_source_path(
            project_root,
            cell_resolver,
            node.label().pkg().as_cell_path().to_owned(),
        )? {
            dirs.insert(path);
        }
    }

    let mut sets: Vec<DevWatchSet> = watch
        .iter()
        .zip(set_targets)
        .map(|(spec, nodes)| DevWatchSet {
            build: spec.build.clone(),
            restart: spec.restart,
            notify: spec.notify,
            files: Vec::new(),
            targets_digest: targets_digest(nodes),
        })
        .collect();
    for (path, set) in files {
        sets[set].files.push(path);
    }
    for set in &mut sets {
        set.files.sort();
    }

    let bzl_files = loaded_files(project_root, dice, cell_resolver, packages).await?;

    Ok(DevWatched {
        watch: sets,
        bzl_files,
        build_files: build_files
            .into_iter()
            .map(|(path, waits)| DevBuildFile { path, waits })
            .collect(),
        dirs: dirs.into_iter().collect(),
        incomplete,
    })
}

fn claim<K: Hash + Eq>(map: &mut BuckMutMap<K, usize>, key: K, set: usize) {
    let entry = map.entry(key).or_insert(set);
    *entry = (*entry).min(set);
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

/// The absolute path of a source path, or `None` for a path under `buck-out`, such as a file
/// of a bundled external cell, which cannot be edited.
fn abs_source_path(
    project_root: &ProjectRoot,
    cell_resolver: &CellResolver,
    path: CellPath,
) -> buck2_error::Result<Option<String>> {
    let rel = cell_resolver.resolve_path(path)?;
    if is_buck_out(&rel) {
        return Ok(None);
    }
    Ok(Some(project_root.resolve(&rel).to_string()))
}

fn is_buck_out(path: &ProjectRelativePath) -> bool {
    path.as_str() == "buck-out" || path.as_str().starts_with("buck-out/")
}

/// The `.bzl` files that the build files of `packages` load, directly or through other `.bzl`
/// files.
async fn loaded_files(
    project_root: &ProjectRoot,
    dice: &mut DiceComputations<'_>,
    cell_resolver: &CellResolver,
    packages: BTreeSet<buck2_core::package::PackageLabel>,
) -> buck2_error::Result<Vec<String>> {
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
    let mut files = Vec::new();
    for import in seen {
        if let Some(path) = abs_source_path(project_root, cell_resolver, import.path().to_owned())?
        {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}
