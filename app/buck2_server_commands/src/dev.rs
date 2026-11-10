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
//! A `DevInfo` says what a change is for in one of two ways, a `Policy`. With `watch`, each
//! file belongs to the first watch set that claims it. Without it, the outputs carry the
//! policy (the `only` proposal of `dev_review.md`, a prototype): the server and each output
//! claim the files that they may be built from, and a file may belong to several of them.
//! Either way, what claims files is a list of `Slot`s, and a set of slots is a bit mask.
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
use buck2_cli_proto::new_generic::DevOutputChanges;
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
use buck2_directory::directory::entry::DirectoryEntry;
use buck2_error::BuckErrorContext;
use buck2_error::internal_error;
use buck2_execute::artifact::fs::ExecutorFs;
use buck2_execute::artifact_value::ArtifactValue;
use buck2_execute::directory::ActionDirectoryMember;
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
    #[error("`buck2 dev` takes at most {MAX_SLOTS} {0}, but the `DevInfo` has {1}")]
    TooManySlots(&'static str, usize),
}

/// How many slots a `DevInfo` may have: a set of slots is the bits of a `u64`.
const MAX_SLOTS: usize = 64;

/// Something that claims source files.
struct Slot {
    /// What it is, for messages.
    what: String,
    /// Target patterns whose own source files the slot claims. `None` means every target of
    /// the graph.
    targets: Option<Vec<String>>,
    /// Artifacts. The slot claims the targets that they are built from, with their source
    /// files.
    inputs: Vec<ArtifactGroup>,
    /// For an output with `only`: the target patterns. A change to a file that the slot claims
    /// holds the output back, unless it is an own source file of one of these targets.
    only: Option<Vec<String>>,
    /// A change to a file of the slot waits: a `notify` watch set.
    waits: bool,
}

/// How a `DevInfo` says what a change is for.
enum Policy {
    /// `DevInfo.watch`. Each watch set is a slot, and a file belongs to the first slot that
    /// claims it.
    Watch(Vec<DevWatchSpec>),
    /// No `DevInfo.watch`: the outputs carry the policy. Slot 0 is the server, and slot
    /// `i + 1` is the output with the `i`th of these names. A file belongs to every slot that
    /// claims it.
    Outputs(Vec<String>),
}

impl Policy {
    fn exclusive(&self) -> bool {
        matches!(self, Policy::Watch(_))
    }
}

/// The slot of the server, for a `Policy::Outputs`.
const SERVER_SLOT: u64 = 1;

fn slot_bit(slot: usize) -> u64 {
    1 << slot
}

/// The lowest slot of `mask`, which must have one.
fn first_slot(mask: u64) -> usize {
    mask.trailing_zeros() as usize
}

fn dev_policy(info: &DevInfo<'_>) -> buck2_error::Result<(Policy, Vec<Slot>)> {
    if let Some(watch) = info.watch()? {
        if watch.len() > MAX_SLOTS {
            return Err(DevError::TooManySlots("watch sets", watch.len()).into());
        }
        let slots = watch
            .iter()
            .enumerate()
            .map(|(set, spec)| Slot {
                what: format!("`DevInfo.watch[{set}].inputs`"),
                targets: spec.targets.clone(),
                inputs: spec.inputs.clone(),
                only: None,
                waits: spec.notify,
            })
            .collect();
        return Ok((Policy::Watch(watch), slots));
    }
    let outputs = info.outputs()?;
    if outputs.len() + 1 > MAX_SLOTS {
        return Err(DevError::TooManySlots("outputs and the server", outputs.len() + 1).into());
    }
    let only = info.only();
    let mut visitor = SimpleCommandLineArtifactVisitor::new();
    info.server_command_line().visit_artifacts(&mut visitor)?;
    let mut slots = vec![Slot {
        what: "`DevInfo.server`".to_owned(),
        targets: Some(Vec::new()),
        inputs: visitor.inputs.into_iter().collect(),
        only: None,
        waits: false,
    }];
    let mut names = Vec::new();
    for (name, artifact) in outputs.iter() {
        slots.push(Slot {
            what: format!("Output `{name}`"),
            targets: Some(Vec::new()),
            inputs: vec![ArtifactGroup::Artifact(artifact.dupe())],
            only: only.get(name).cloned(),
            waits: false,
        });
        names.push(name.clone());
    }
    Ok((Policy::Outputs(names), slots))
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
            let (server, digest) = {
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
                let mut contents = Vec::new();
                for (_, values) in &ensured {
                    for (artifact, value) in values.iter() {
                        let path = artifact
                            .resolve_path(&artifact_fs, Some(&value.content_based_path_hash()))?;
                        contents.push((path.to_string(), value_digest(value)));
                    }
                }
                let digest = contents_digest(contents);
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
                (args, digest)
            };
            Ok(DevResponse::Server(DevServer {
                project_root: server_ctx.project_root().root().to_string(),
                server,
                digest,
                outputs: outputs.keys().cloned().collect(),
                interactive: info.as_ref().value().as_ref().interactive(),
            }))
        }
        DevAction::Watch(request) => {
            let (policy, slots) = dev_policy(info.as_ref().value().as_ref())?;
            Ok(DevResponse::Watch(
                watch_files(
                    &mut ctx.ctx(),
                    &label,
                    &policy,
                    &slots,
                    request,
                    &req.session,
                )
                .await?,
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
            // The slots that this build is built from. A change to a file of one of them stops
            // it.
            let (policy, _) = dev_policy(info.as_ref().value().as_ref())?;
            let builds = match &policy {
                Policy::Watch(watch) => watch
                    .iter()
                    .enumerate()
                    .filter(|(_, spec)| !spec.build.is_empty() || spec.restart)
                    .fold(0, |mask, (set, _)| mask | slot_bit(set)),
                Policy::Outputs(all) => all
                    .iter()
                    .enumerate()
                    .filter(|(_, name)| names.contains(name))
                    .fold(0, |mask, (output, _)| mask | slot_bit(output + 1)),
            };
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
                            interrupted = changed_files(dice, changes, claims, builds, io) => {
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
    builds: u64,
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
            if builds_from_changed(dice, claims, builds, &io, &path)
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

/// Whether the build, of the slots `builds`, is built from `path`, and the file differs from
/// what this command saw.
async fn builds_from_changed(
    dice: &mut DiceComputations<'_>,
    claims: &DevClaims,
    builds: u64,
    io: &Arc<dyn IoProvider>,
    path: &ProjectRelativePath,
) -> buck2_error::Result<bool> {
    let cell_resolver = dice.get_cell_resolver().await?;
    let relevant = match claims.classify(dice, cell_resolver, path).await? {
        // With `only`, a change that holds an output back stops a build of it too: the build
        // would use the change.
        Claimed::Source { may, .. } => claims.slots_of(may) & builds != 0,
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
    /// Whether a file belongs to the first slot that claims it, and not to every one.
    exclusive: bool,
    /// The targets of the graph.
    nodes: Vec<ConfiguredTargetNode>,
    /// The source files that slots claim by name: those that the actions of the targets that
    /// own their `inputs` use, and those that claimed symlinks point at.
    files: BuckMutMap<ProjectRelativePathBuf, (u64, CellPath)>,
    /// The slots that claim the own source files of each target.
    with_own_files: BuckMutMap<ConfiguredTargetLabel, u64>,
    /// The slots whose `only` names each target.
    only: BuckMutMap<ConfiguredTargetLabel, u64>,
    /// The targets whose digests say whether the targets of the slots changed, in groups.
    /// When exclusive, group `i` has the targets of slot `i`. Otherwise group `2 * i` has the
    /// targets of slot `i` that its `only` names, or all of them if it has none, and group
    /// `2 * i + 1` has the others.
    groups: Vec<Vec<ConfiguredTargetNode>>,
    /// How many targets a slot has.
    claimed_targets: usize,
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
    /// The slots that claim the source file `path` of a target whose own source files `own`
    /// claim: those, and the ones that claim the file by name.
    fn slots_of_own_file(&self, cell_resolver: &CellResolver, path: CellPath, own: u64) -> u64 {
        match project_path(cell_resolver, path) {
            Ok(Some(rel)) => match self.files.get(&rel) {
                Some((slots, _)) => own | *slots,
                None => own,
            },
            _ => own,
        }
    }
}

/// Walk the configured dependency graph of `label`. Give each target to the slots that claim
/// it.
///
/// This reads the contents of no source file, so that a `DevClaims` made from it stays valid
/// when one changes.
async fn graph_claims(
    dice: &mut DiceComputations<'_>,
    label: &ConfiguredProvidersLabel,
    exclusive: bool,
    slots: &[Slot],
) -> buck2_error::Result<GraphClaims> {
    let mut timer = Timer::new();
    let cell_resolver = dice.get_cell_resolver().await?;
    let cell = label.target().pkg().cell_name();
    let cell_alias_resolver = dice.get_cell_alias_resolver(cell).await?;

    let parse = |targets: &Option<Vec<String>>| {
        targets
            .as_ref()
            .map(|targets| {
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
                    .collect::<buck2_error::Result<Vec<_>>>()
            })
            .transpose()
    };
    let mut patterns: Vec<Option<Vec<ParsedPattern<TargetPatternExtra>>>> =
        Vec::with_capacity(slots.len());
    let mut only_patterns: Vec<Option<Vec<ParsedPattern<TargetPatternExtra>>>> =
        Vec::with_capacity(slots.len());
    for slot in slots {
        patterns.push(parse(&slot.targets)?);
        only_patterns.push(parse(&slot.only)?);
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

    // The slots that claim each target, and each source file that slots claim by name. A slot
    // claims the targets that its `targets` match, with their own source files. Of the targets
    // that own its `inputs`, it claims only the source files that their actions use. What
    // those actions take from other targets gives it those targets and their dependencies,
    // with their own source files. The targets of a slot say which slots a build file change
    // changed.
    let mut files: BuckMutMap<ProjectRelativePathBuf, (u64, CellPath)> = BuckMutMap::default();
    let mut targets: BuckMutMap<ConfiguredTargetLabel, u64> = BuckMutMap::default();
    let mut with_own_files: BuckMutMap<ConfiguredTargetLabel, u64> = BuckMutMap::default();
    let mut only: BuckMutMap<ConfiguredTargetLabel, u64> = BuckMutMap::default();
    let mut only_slots = 0;
    let mut incomplete = Vec::new();
    for (set, spec) in slots.iter().enumerate() {
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
        if let Some(p) = &only_patterns[set] {
            only_slots |= slot_bit(set);
            for node in &nodes {
                if p.iter().any(|p| p.matches(node.label().unconfigured())) {
                    claim(&mut only, node.label().dupe(), set);
                }
            }
        }
        if spec.inputs.is_empty() {
            continue;
        }
        let walk = walk_inputs(dice, &spec.inputs).await;
        for path in walk.sources {
            claim_file(&mut files, cell_resolver, path, slot_bit(set))?;
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
            incomplete.push(format!("{}: {error:#}", spec.what));
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

    let notify_sets = slots.iter().any(|slot| slot.waits);
    let mut groups: Vec<Vec<ConfiguredTargetNode>> = vec![
        Vec::new();
        if exclusive {
            slots.len()
        } else {
            2 * slots.len()
        }
    ];
    let mut claimed_targets = 0;
    // Each build file, with whether a change to it waits.
    let mut build_files: BTreeMap<ProjectRelativePathBuf, (bool, CellPath)> = BTreeMap::new();
    let mut packages = BTreeSet::new();
    let mut package_dirs = BTreeSet::new();
    for node in &nodes {
        let has = targets.get(node.label()).copied().unwrap_or(0);
        // When exclusive, the first slot that has the target.
        let set = (exclusive && has != 0).then(|| first_slot(has));
        if has != 0 {
            claimed_targets += 1;
        }
        if let Some(set) = set {
            groups[set].push(node.dupe());
        } else {
            let named = only.get(node.label()).copied().unwrap_or(0);
            for slot in 0..slots.len() {
                if has & slot_bit(slot) == 0 {
                    continue;
                }
                let outside = only_slots & slot_bit(slot) != 0 && named & slot_bit(slot) == 0;
                groups[2 * slot + outside as usize].push(node.dupe());
            }
        }
        if packages.insert(node.label().pkg()) {
            if let Some(dir) = project_path(cell_resolver, node.label().pkg().as_cell_path())? {
                package_dirs.insert(dir);
            }
        }
        let buildfile = node.buildfile_path().path();
        if let Some(path) = project_path(cell_resolver, buildfile.dupe())? {
            build_files.entry(path).or_insert((false, buildfile)).0 |=
                set.is_some_and(|set| slots[set].waits);
        }
    }
    timer.step(&format!("the {} packages of the graph", packages.len()));

    let mut claims = GraphClaims {
        exclusive,
        nodes,
        files,
        with_own_files,
        only,
        groups,
        claimed_targets,
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

    // The source files that are symlinks, or behind one, with the slots that claim them.
    let mut links: Vec<(u64, CellPath)> = Vec::new();
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
                links.push((claims.slots_of_own_file(cell_resolver, input, *own), input));
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
            // New to some of the slots.
            let new = claims
                .files
                .get(&rel)
                .is_none_or(|(slots, _)| set & !*slots != 0);
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

/// What a path is to the slots.
enum Claimed {
    /// A source file that slots claim.
    Source {
        /// The slots that claim it, at least one.
        may: u64,
        /// Of those, the ones whose `only` names a target that has the file as a source file.
        only: u64,
    },
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
    /// Whether a file belongs to the first slot that claims it, and not to every one.
    exclusive: bool,
    /// For each target of the graph, by the hash of its unconfigured label: the slots that
    /// claim its own source files, and the slots whose `only` names it. Sorted.
    targets: Vec<(u64, u64, u64)>,
    /// The source files that slots claim by name, by the hash of their project-relative path,
    /// with the slots. Sorted.
    files: Vec<(u64, u64)>,
    /// The packages of the graph, and the files that matter outside them: what the session's
    /// file change subscription needs to drop the changes that cannot matter.
    interest: Arc<FileInterest>,
    /// The build files of the graph, with whether a change to them waits. Sorted.
    build_files: Vec<(u64, bool)>,
    /// The `.bzl` files that the graph loads. Sorted.
    bzl_files: Vec<u64>,
    /// A digest of each group of targets; see `GraphClaims::groups`.
    targets_digests: Vec<String>,
    /// How many targets each group has.
    target_counts: Vec<usize>,
    /// How many targets a slot has.
    claimed_targets: usize,
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
            exclusive,
            nodes: _,
            files,
            with_own_files,
            only,
            groups,
            claimed_targets,
            package_dirs,
            build_files,
            bzl_files,
            notify_sets,
            incomplete,
        } = claims;

        let mut by_label: BTreeMap<u64, (u64, u64)> = BTreeMap::new();
        for (label, slots) in &with_own_files {
            by_label
                .entry(stable_hash(&label.unconfigured().to_string()))
                .or_default()
                .0 |= *slots;
        }
        for (label, slots) in &only {
            by_label
                .entry(stable_hash(&label.unconfigured().to_string()))
                .or_default()
                .1 |= *slots;
        }

        let mut by_path: BTreeMap<u64, u64> = BTreeMap::new();
        for (path, (slots, _)) in &files {
            *by_path.entry(stable_hash(path.as_str())).or_default() |= *slots;
        }
        let other_files: Vec<(u64, u64)> = by_path.into_iter().collect();

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

        let targets_digests: Vec<String> = groups
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
            exclusive,
            targets: by_label
                .into_iter()
                .map(|(hash, (own, only))| (hash, own, only))
                .collect(),
            files: other_files,
            interest,
            build_files: build_file_hashes,
            bzl_files: bzl_hashes,
            targets_digests,
            target_counts: groups.iter().map(|nodes| nodes.len()).collect(),
            claimed_targets,
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

    /// The slots that a file of the slots `may` belongs to: the first of them when exclusive,
    /// and otherwise all of them.
    fn slots_of(&self, may: u64) -> u64 {
        if self.exclusive {
            may & may.wrapping_neg()
        } else {
            may
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
        let mut may = match self.files.binary_search_by_key(&hash, |x| x.0) {
            Ok(i) => self.files[i].1,
            Err(_) => 0,
        };
        let mut only = 0;
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
                let (_, own, named) = self.targets[i];
                // Whether the target would add to the answer, if it has the file.
                let adds = if self.exclusive {
                    // A lower slot than the lowest so far.
                    own.trailing_zeros() < may.trailing_zeros()
                } else {
                    own & !may != 0 || named & !only != 0
                };
                if adds && node.inputs().any(|input| input == cell_path) {
                    may |= own;
                    only |= named;
                }
            }
        }
        Ok(if may == 0 {
            Claimed::Nothing
        } else {
            Claimed::Source {
                may,
                only: only & may,
            }
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
        let (policy, slots) = dev_slots(ctx, &self.0).await?;
        let claims = graph_claims(ctx, &self.0, policy.exclusive(), &slots).await?;
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

/// The policy of the `DevInfo` of a target, and its slots.
async fn dev_slots(
    dice: &mut DiceComputations<'_>,
    label: &ConfiguredProvidersLabel,
) -> buck2_error::Result<(Policy, Vec<Slot>)> {
    let providers = dice.get_providers(label).await?.require_compatible()?;
    let info = providers
        .builtin_provider_value::<DevInfo>()?
        .ok_or_else(|| DevError::NoDevInfo(label.dupe()))?;
    dev_policy(info.as_ref().value().as_ref())
}

/// Say what the requested paths are to the watch sets, and which sets' targets changed since
/// `request.previous`. Whether a file changed is not said here: a path that `buck2 dev` asks
/// about did.
async fn watch_files(
    dice: &mut DiceComputations<'_>,
    label: &ConfiguredProvidersLabel,
    policy: &Policy,
    slots: &[Slot],
    request: &DevWatchRequest,
    session: &str,
) -> buck2_error::Result<DevWatched> {
    let watch: &[DevWatchSpec] = match policy {
        Policy::Watch(watch) => watch,
        Policy::Outputs(_) => &[],
    };
    let mut outputs = match policy {
        Policy::Watch(_) => None,
        Policy::Outputs(names) => Some(OutputChanges::new(names, slots)),
    };
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
            let claimed = claims.classify(dice, cell_resolver, rel).await?;
            match (&mut outputs, claimed) {
                (Some(outputs), claimed) => {
                    outputs.path(path, &claimed);
                    (None, false)
                }
                (None, Claimed::Source { may, .. }) => {
                    let set = first_slot(may);
                    (Some(set), watch[set].notify)
                }
                (None, Claimed::BuildFile { waits }) => (None, waits),
                (None, Claimed::Bzl) => (None, claims.notify_sets),
                (None, Claimed::Nothing) => (None, false),
            }
        } else {
            (None, false)
        };
        paths.push(DevPath {
            path: path.clone(),
            set,
            waits,
        });
    }

    // The build files and `.bzl` files whose change waits, for when the analysis fails and
    // cannot say what a path is.
    let mut waiting = Vec::new();
    if request.all_waiting && claims.notify_sets {
        let graph = graph_claims(dice, label, policy.exclusive(), slots).await?;
        waiting.extend(
            graph
                .build_files
                .iter()
                .filter(|(_, (waits, _))| *waits)
                .map(|(path, _)| path.to_string()),
        );
        waiting.extend(graph.bzl_files.keys().map(|path| path.to_string()));
    }

    let state = claims.state();
    // Each of these is a group of targets; see `GraphClaims::groups`.
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

    let outputs = outputs.map(|mut outputs| {
        if request.all_waiting && request.previous.is_some() {
            outputs.missed();
        }
        outputs.finish(&changed_sets, graph_changed, &claims)
    });
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
        waiting,
        changed_sets: if outputs.is_some() {
            Vec::new()
        } else {
            changed_sets
        },
        graph_changed,
        incomplete: claims.incomplete.clone(),
        outputs,
    })
}

/// Gathers what changes are to the server and to the outputs that carry the policy.
struct OutputChanges<'a> {
    names: &'a [String],
    /// The slots that have `only`.
    only_slots: u64,
    changes: DevOutputChanges,
    /// The requested paths that are build files or `.bzl` files of the graph.
    graph_paths: Vec<String>,
}

impl<'a> OutputChanges<'a> {
    fn new(names: &'a [String], slots: &[Slot]) -> Self {
        let only_slots = slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.only.is_some())
            .fold(0, |mask, (slot, _)| mask | slot_bit(slot));
        OutputChanges {
            names,
            only_slots,
            changes: DevOutputChanges::default(),
            graph_paths: Vec::new(),
        }
    }

    /// Changes were missed, so anything may have changed: the server may be new, every output
    /// without `only` is built again, and the others are held back.
    fn missed(&mut self) {
        self.changes.server = true;
        for (slot, name) in self.names.iter().enumerate().map(|(i, name)| (i + 1, name)) {
            if self.only_slots & slot_bit(slot) != 0 {
                self.changes
                    .hold
                    .entry(name.clone())
                    .or_default()
                    .push(String::new());
            } else {
                self.changes.build.entry(name.clone()).or_default();
            }
        }
    }

    /// The requested path `path` changed.
    fn path(&mut self, path: &str, claimed: &Claimed) {
        let (may, only) = match claimed {
            Claimed::Source { may, only } => (*may, *only),
            Claimed::BuildFile { .. } | Claimed::Bzl => {
                self.graph_paths.push(path.to_owned());
                return;
            }
            Claimed::Nothing => return,
        };
        self.changes.server |= may & SERVER_SLOT != 0;
        for (slot, name) in self.names.iter().enumerate().map(|(i, name)| (i + 1, name)) {
            let bit = slot_bit(slot);
            if may & bit == 0 {
                continue;
            }
            // A file outside the `only` of an output holds it back.
            let changes = if self.only_slots & bit != 0 && only & bit == 0 {
                &mut self.changes.hold
            } else {
                &mut self.changes.build
            };
            changes
                .entry(name.clone())
                .or_default()
                .push(path.to_owned());
        }
    }

    /// The groups of targets `changed_groups` changed: group `2 * slot` has the targets of a
    /// slot that its `only` names, or all of them if it has none, and group `2 * slot + 1` has
    /// the others.
    fn finish(
        mut self,
        changed_groups: &[usize],
        graph_changed: bool,
        claims: &DevClaims,
    ) -> DevOutputChanges {
        self.changes.server |= graph_changed;
        for group in changed_groups {
            let (slot, outside) = (group / 2, group % 2 == 1);
            if slot == 0 {
                self.changes.server = true;
                continue;
            }
            let Some(name) = self.names.get(slot - 1) else {
                continue;
            };
            // Without `only`, an output has no targets outside it.
            let changes = if outside && self.only_slots & slot_bit(slot) != 0 {
                &mut self.changes.hold
            } else {
                &mut self.changes.build
            };
            changes
                .entry(name.clone())
                .or_default()
                .extend(self.graph_paths.iter().cloned());
        }
        for paths in self
            .changes
            .build
            .values_mut()
            .chain(self.changes.hold.values_mut())
        {
            paths.sort();
            paths.dedup();
        }
        self.changes.targets = claims.claimed_targets;
        self.changes
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

/// The contents of a built artifact: of a file, of a directory with all that is in it, or
/// where a symlink points, and of what the symlinks in it point to.
fn value_digest(value: &ArtifactValue) -> String {
    let entry = match value.entry() {
        DirectoryEntry::Dir(dir) => dir.fingerprint().data().to_string(),
        DirectoryEntry::Leaf(ActionDirectoryMember::File(file)) => format!(
            "{}{}",
            file.digest.data(),
            if file.is_executable { "+x" } else { "" }
        ),
        DirectoryEntry::Leaf(ActionDirectoryMember::Symlink(symlink)) => {
            format!("-> {:?}", symlink.target())
        }
        DirectoryEntry::Leaf(ActionDirectoryMember::ExternalSymlink(symlink)) => {
            format!("-> {:?}", symlink.to_path_buf())
        }
    };
    match value.deps() {
        Some(deps) => format!("{} with {}", entry, deps.fingerprint().data()),
        None => entry,
    }
}

/// A digest of artifacts, by their paths and their contents, which is the same in another
/// daemon.
fn contents_digest(mut contents: Vec<(String, String)>) -> String {
    contents.sort();
    contents.dedup();
    let mut hasher = DefaultHasher::new();
    contents.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
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

/// Slot `slot` claims `key`.
fn claim<K: Hash + Eq>(map: &mut BuckMutMap<K, u64>, key: K, slot: usize) {
    *map.entry(key).or_insert(0) |= slot_bit(slot);
}

/// The slots `slots` claim the file `path`.
fn claim_file(
    files: &mut BuckMutMap<ProjectRelativePathBuf, (u64, CellPath)>,
    cell_resolver: &CellResolver,
    path: CellPath,
    slots: u64,
) -> buck2_error::Result<Option<ProjectRelativePathBuf>> {
    let Some(rel) = project_path(cell_resolver, path.dupe())? else {
        return Ok(None);
    };
    files.entry(rel.clone()).or_insert((0, path)).0 |= slots;
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
