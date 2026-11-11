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
//! - `Watch` says what each changed path is to the server and to each output, and whether
//!   their targets changed.
//! - `Build` builds named `DevInfo` outputs.
//!
//! The client runs the server, and acts on the file changes that the daemon's file watcher pushes
//! to it through a subscription, so no daemon command lasts longer than one build.
//!
//! The server and each output claim the files that they may be built from, and a file may
//! belong to several of them. What claims files is a list of `Slot`s, and a set of slots is a
//! bit mask. Each change also says which of `DevInfo.scopes` it is in: those of the targets that
//! it is to, or that have the changed file as an own source file.
//!
//! What the slots claim is a DICE key, `DevClaimsKey`. Finding it walks the targets of the
//! graph, and the actions of the targets that own the artifacts of the slots, which builds what
//! dynamic actions of those targets need to say what they do. It reads the contents of no source
//! file, so DICE keeps it while only source files change: a `Watch` or a `Build` then looks up
//! the changed paths in it, and what that costs does not grow with the graph. Its value says
//! which slot has which target, and lists no source files; see `DevClaims`.
//!
//! A dynamic output, which the server defines while it runs, is a call of a handler of
//! `DevInfo.handlers`: a bxl function with arguments. Each has claims of its own, a
//! `DevDynamicClaimsKey`, with one output slot. `buck2 dev` sends the dynamic outputs that it
//! tracks with each `Watch`, and the session's file interest is the union of all the claims.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::VecDeque;
use std::hash::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use allocative::Allocative;
use async_trait::async_trait;
use buck2_artifact::actions::key::ActionKey;
use buck2_artifact::artifact::artifact_dyn::ArtifactDyn;
use buck2_build_api::actions::artifact::get_artifact_fs::GetArtifactFs;
use buck2_build_api::actions::calculation::ActionCalculation;
use buck2_build_api::analysis::calculation::RuleAnalysisCalculation;
use buck2_build_api::artifact_groups::ArtifactGroup;
use buck2_build_api::artifact_groups::ArtifactGroupValues;
use buck2_build_api::artifact_groups::ResolvedArtifactGroup;
use buck2_build_api::artifact_groups::TransitiveSetProjectionKey;
use buck2_build_api::bxl::calculation::BXL_CALCULATION_IMPL;
use buck2_build_api::interpreter::rule_defs::cmd_args::ArtifactPathMapperImpl;
use buck2_build_api::interpreter::rule_defs::cmd_args::CommandLineArgLike;
use buck2_build_api::interpreter::rule_defs::cmd_args::CommandLineBuilder;
use buck2_build_api::interpreter::rule_defs::cmd_args::SimpleCommandLineArtifactVisitor;
use buck2_build_api::interpreter::rule_defs::provider::builtin::dev_info::DevArtifact;
use buck2_build_api::interpreter::rule_defs::provider::builtin::dev_info::DevInfo;
use buck2_build_api::interpreter::rule_defs::provider::builtin::dev_info::DevOutputSpec;
use buck2_build_api::interpreter::rule_defs::provider::builtin::dev_info::OwnedDevInfo;
use buck2_build_api::interpreter::rule_defs::provider::builtin::local_resource_info::LocalResourceInfo;
use buck2_build_api::interpreter::rule_defs::provider::builtin::local_resource_info::OwnedLocalResourceInfo;
use buck2_build_api::interpreter::rule_defs::provider::builtin::run_info::OwnedRunInfo;
use buck2_build_api::interpreter::rule_defs::provider::builtin::run_info::RunInfo;
use buck2_build_api::interpreter::rule_defs::provider::collection::FrozenProviderCollectionValue;
use buck2_build_api::materialize::HasMaterializationQueueTracker;
use buck2_build_api::materialize::MaterializationAndUploadContext;
use buck2_build_api::materialize::materialize_and_upload_artifact_group;
use buck2_cli_proto::new_generic::DevAction;
use buck2_cli_proto::new_generic::DevBuilt;
use buck2_cli_proto::new_generic::DevBuiltOutput;
use buck2_cli_proto::new_generic::DevBuiltSidecar;
use buck2_cli_proto::new_generic::DevChange;
use buck2_cli_proto::new_generic::DevDynamicCall;
use buck2_cli_proto::new_generic::DevDynamicWatched;
use buck2_cli_proto::new_generic::DevInterrupted;
use buck2_cli_proto::new_generic::DevOutputChanges;
use buck2_cli_proto::new_generic::DevRequest;
use buck2_cli_proto::new_generic::DevResponse;
use buck2_cli_proto::new_generic::DevServer;
use buck2_cli_proto::new_generic::DevTargets;
use buck2_cli_proto::new_generic::DevWatchRequest;
use buck2_cli_proto::new_generic::DevWatchState;
use buck2_cli_proto::new_generic::DevWatched;
use buck2_cli_proto::new_generic::NewGenericPartialResult;
use buck2_common::dice::cells::HasCellResolver;
use buck2_common::file_ops::dice::DiceFileComputations;
use buck2_common::file_ops::metadata::FileType;
use buck2_common::file_ops::metadata::RawPathMetadata;
use buck2_common::file_ops::metadata::RawSymlink;
use buck2_common::io::IoProvider;
use buck2_common::package_listing::dice::DicePackageListingResolver;
use buck2_common::package_listing::resolver::PackageListingResolver;
use buck2_common::pattern::parse_from_cli::parse_patterns_from_cli_args;
use buck2_core::bxl::BxlFilePath;
use buck2_core::bzl::ImportPath;
use buck2_core::cells::CellResolver;
use buck2_core::cells::cell_path::CellPath;
use buck2_core::cells::name::CellName;
use buck2_core::deferred::base_deferred_key::BaseDeferredKey;
use buck2_core::deferred::base_deferred_key::BaseDeferredKeyBxl;
use buck2_core::execution_types::executor_config::PathSeparatorKind;
use buck2_core::fs::artifact_path_resolver::ArtifactFs;
use buck2_core::fs::i_project_rel_path::IProjectRelativePath;
use buck2_core::fs::project::ProjectRoot;
use buck2_core::fs::project_rel_path::ProjectRelativePath;
use buck2_core::fs::project_rel_path::ProjectRelativePathBuf;
use buck2_core::global_cfg_options::GlobalCfgOptions;
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
use buck2_hash::BuckMutInsertionMap;
use buck2_hash::BuckMutInsertionSet;
use buck2_hash::BuckMutMap;
use buck2_hash::BuckMutSet;
use buck2_interpreter::load_module::InterpreterCalculation;
use buck2_interpreter::paths::module::StarlarkModulePath;
use buck2_node::nodes::configured::ConfiguredTargetNode;
use buck2_node::nodes::configured_frontend::ConfiguredTargetNodeCalculation;
use buck2_node::nodes::frontend::TargetGraphCalculation;
use buck2_node::target_calculation::ConfiguredTargetCalculation;
use buck2_server_ctx::bxl::BXL_SERVER_COMMANDS;
use buck2_server_ctx::ctx::FileChangeBatch;
use buck2_server_ctx::ctx::ServerCommandContextTrait;
use buck2_server_ctx::dev_interest::FileInterest;
use buck2_server_ctx::dev_interest::add_interest;
use buck2_server_ctx::dev_interest::clear_interest;
use buck2_server_ctx::dev_interest::session_held;
use buck2_server_ctx::dev_interest::session_held_if_any;
use buck2_server_ctx::dev_interest::set_interest;
use buck2_server_ctx::dev_interest::stable_hash;
use buck2_server_ctx::global_cfg_options::global_cfg_options_from_client_context;
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

use crate::dev_resources::ResourcesUpdate;
use crate::dev_resources::SessionResources;
use crate::dev_resources::SetupKey;
use crate::dev_resources::WantedResource;

#[derive(Debug, buck2_error::Error)]
#[buck2(tag = Input)]
enum DevError {
    #[error("`buck2 dev` takes exactly one target, but `{0}` matched {1}")]
    NotOneTarget(String, usize),
    #[error(
        "{0} has no `DevInfo` provider and no `RunInfo` provider, so `buck2 dev` cannot use it"
    )]
    NoDevInfo(DevSubject),
    #[error(
        "The bxl function `{0}` returned no providers. For `buck2 dev`, it returns a list of providers, as a rule does, with a `DevInfo` or a `RunInfo`"
    )]
    NoProviders(DevSubject),
    #[error("{subject} has no output named `{name}` in its `DevInfo`")]
    UnknownOutput { subject: DevSubject, name: String },
    #[error("Output `{0}` of the `DevInfo` produced no artifact")]
    EmptyOutput(String),
    #[error(
        "`DevInfo.local_resources` names `{target}` for `{name}`, which has no `LocalResourceInfo` provider"
    )]
    NoLocalResourceInfo {
        name: String,
        target: ConfiguredProvidersLabel,
    },
    #[error(
        "The `buck2 dev` session `{0}` has no file change subscription, so the daemon cannot hold its local resources"
    )]
    NoSession(String),
    #[error("`buck2 dev` takes at most {MAX_SLOTS} {0}, but the `DevInfo` has {1}")]
    TooManySlots(&'static str, usize),
    #[error("{subject} has no handler named `{name}` in its `DevInfo`")]
    UnknownHandler { subject: DevSubject, name: String },
    #[error("`{0}` is an output of the `DevInfo`, so it cannot be the name of a dynamic output")]
    DynamicNameTaken(String),
}

/// How many slots a `DevInfo` may have: a set of slots is the bits of a `u64`.
const MAX_SLOTS: usize = 64;

/// Something that claims source files: the server, or an output.
struct Slot {
    /// What it is, for messages.
    what: String,
    /// Artifacts. The slot claims the targets that they are built from, with their source
    /// files.
    inputs: Vec<ArtifactGroup>,
}

/// What `buck2 dev` needs of a `DevInfo` to say what a change is to it.
struct Policy {
    /// The names of the outputs. Slot 0 is the server, and slot `i + 1` is the output with the
    /// `i`th of these names.
    names: Vec<String>,
    slots: Vec<Slot>,
    /// `DevInfo.scopes`: each name with its target patterns. A set of scopes is the bits of a
    /// `u64`, by this order.
    scopes: Vec<(String, Vec<String>)>,
}

impl Policy {
    /// The names of the scopes in `mask`.
    fn scope_names(&self, mask: u64) -> Vec<String> {
        self.scopes
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & slot_bit(*i) != 0)
            .map(|(_, (name, _))| name.clone())
            .collect()
    }
}

fn slot_bit(slot: usize) -> u64 {
    1 << slot
}

/// What a session runs: the providers of a target, or those that a bxl function returns, as a
/// rule does.
#[derive(Clone, Dupe, Debug, PartialEq, Allocative, Pagable)]
enum DevSubject {
    Target(ConfiguredProvidersLabel),
    Bxl {
        key: BaseDeferredKeyBxl,
        /// The file of the function. It and what it loads count as `.bzl` files of the graph.
        file: CellPath,
    },
}

impl Eq for DevSubject {}

impl Hash for DevSubject {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            DevSubject::Target(label) => label.hash(state),
            DevSubject::Bxl { key, file } => {
                key.0.hash().hash(state);
                file.hash(state);
            }
        }
    }
}

impl std::fmt::Display for DevSubject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DevSubject::Target(label) => write!(f, "Target `{label}`"),
            DevSubject::Bxl { key, .. } => write!(f, "The bxl function `{key}`"),
        }
    }
}

impl DevSubject {
    /// The target that the command line names, or the bxl function with its arguments.
    async fn of_request(
        server_ctx: &dyn ServerCommandContextTrait,
        ctx: &DiceTransaction,
        req: &DevRequest,
        global_cfg_options: &GlobalCfgOptions,
    ) -> buck2_error::Result<DevSubject> {
        if let Some(bxl_args) = &req.bxl_args {
            return DevSubject::bxl(server_ctx, ctx, &req.target, bxl_args, global_cfg_options)
                .await;
        }
        let mut dice = ctx.ctx();
        let patterns = parse_patterns_from_cli_args::<ProvidersPatternExtra>(
            &mut dice,
            std::slice::from_ref(&req.target),
            server_ctx.working_dir(),
        )
        .await?;
        let [pattern] = patterns.as_slice() else {
            return Err(DevError::NotOneTarget(req.target.clone(), patterns.len()).into());
        };
        let label = pattern.clone().as_providers_label(&req.target)?;
        Ok(DevSubject::Target(
            dice.get_configured_provider_label(&label, global_cfg_options)
                .await?,
        ))
    }

    /// The bxl function `label`, `path.bxl:name`, called with the command line arguments `args`.
    async fn bxl(
        server_ctx: &dyn ServerCommandContextTrait,
        ctx: &DiceTransaction,
        label: &str,
        args: &[String],
        global_cfg_options: &GlobalCfgOptions,
    ) -> buck2_error::Result<DevSubject> {
        let (key, file) = BXL_SERVER_COMMANDS
            .get()?
            .bxl_key(server_ctx, ctx, label, args, global_cfg_options)
            .await?;
        Ok(DevSubject::Bxl {
            key,
            file: file.path().dupe(),
        })
    }

    /// The providers of the target, or those that the bxl function returned.
    async fn providers(
        &self,
        dice: &mut DiceComputations<'_>,
    ) -> buck2_error::Result<FrozenProviderCollectionValue> {
        match self {
            DevSubject::Target(label) => dice.get_providers(label).await?.require_compatible(),
            DevSubject::Bxl { key, .. } => BXL_CALCULATION_IMPL
                .get()?
                .eval_bxl(dice, key.dupe())
                .await?
                .providers()?
                .ok_or_else(|| DevError::NoProviders(self.dupe()).into()),
        }
    }

    /// The cell that the target patterns of `DevInfo.scopes` are relative to.
    fn cell(&self) -> CellName {
        match self {
            DevSubject::Target(label) => label.target().pkg().cell_name(),
            DevSubject::Bxl { file, .. } => file.cell(),
        }
    }
}

/// What `buck2 dev` runs for a target: its `DevInfo`, or, for a target without one, its
/// `RunInfo`, which is then the server of a `DevInfo` without outputs.
enum DevProvider {
    Dev(OwnedDevInfo),
    Run(OwnedRunInfo),
}

/// A `DevProvider`, borrowed. It must not be held across an await.
#[derive(Clone, Copy)]
enum DevView<'a> {
    Dev(&'a DevInfo<'a>),
    Run(&'a RunInfo<'a>),
}

impl DevProvider {
    fn of(
        providers: &FrozenProviderCollectionValue,
        subject: &DevSubject,
    ) -> buck2_error::Result<DevProvider> {
        if let Some(info) = providers.builtin_provider_value::<DevInfo>()? {
            return Ok(DevProvider::Dev(info));
        }
        if let Some(info) = providers.builtin_provider_value::<RunInfo>()? {
            return Ok(DevProvider::Run(info));
        }
        Err(DevError::NoDevInfo(subject.dupe()).into())
    }

    fn view(&self) -> DevView<'_> {
        match self {
            DevProvider::Dev(info) => DevView::Dev(info.as_ref().value().as_ref()),
            DevProvider::Run(info) => DevView::Run(info.as_ref().value().as_ref()),
        }
    }
}

impl<'a> DevView<'a> {
    fn server_command_line(self) -> &'a dyn CommandLineArgLike<'a> {
        match self {
            DevView::Dev(info) => info.server_command_line(),
            DevView::Run(info) => info,
        }
    }

    fn outputs(self) -> buck2_error::Result<BuckMutInsertionMap<String, DevOutputSpec>> {
        match self {
            DevView::Dev(info) => info.outputs(),
            DevView::Run(_) => Ok(BuckMutInsertionMap::default()),
        }
    }

    /// The command line of output `name`, if it is one.
    fn output_command_line(self, name: &str) -> Option<&'a dyn CommandLineArgLike<'a>> {
        match self {
            DevView::Dev(info) => info.output_command_line(name),
            DevView::Run(_) => None,
        }
    }

    fn sidecars(
        self,
    ) -> buck2_error::Result<BuckMutInsertionMap<String, Vec<(String, DevArtifact)>>> {
        match self {
            DevView::Dev(info) => info.sidecars(),
            DevView::Run(_) => Ok(BuckMutInsertionMap::default()),
        }
    }

    fn scopes(self) -> BuckMutInsertionMap<String, Vec<String>> {
        match self {
            DevView::Dev(info) => info.scopes(),
            DevView::Run(_) => BuckMutInsertionMap::default(),
        }
    }

    fn interactive(self) -> bool {
        match self {
            DevView::Dev(info) => info.interactive(),
            DevView::Run(_) => false,
        }
    }

    /// `DevInfo.handlers`: each name with the label of its bxl function.
    fn handlers(self) -> Vec<(String, String)> {
        match self {
            DevView::Dev(info) => info.handlers(),
            DevView::Run(_) => Vec::new(),
        }
    }

    fn local_resources(self) -> Vec<(String, ConfiguredProvidersLabel)> {
        match self {
            DevView::Dev(info) => info
                .local_resources()
                .into_iter()
                .map(|(name, label)| (name.to_owned(), label.dupe()))
                .collect(),
            DevView::Run(_) => Vec::new(),
        }
    }
}

/// A local resource of the server: a name in `DevInfo.local_resources`, and the target that
/// has its `LocalResourceInfo`.
struct Resource {
    name: String,
    target: ConfiguredProvidersLabel,
    info: OwnedLocalResourceInfo,
}

impl Resource {
    fn setup_command_line(&self) -> &dyn CommandLineArgLike<'_> {
        self.info.as_ref().value().as_ref().setup_command_line()
    }
}

/// The `LocalResourceInfo` of each of `DevInfo.local_resources`.
async fn dev_resources(
    dice: &mut DiceComputations<'_>,
    info: &DevProvider,
) -> buck2_error::Result<Vec<Resource>> {
    let mut resources = Vec::new();
    for (name, target) in info.view().local_resources() {
        let providers = dice.get_providers(&target).await?.require_compatible()?;
        let info = providers
            .builtin_provider_value::<LocalResourceInfo>()?
            .ok_or_else(|| DevError::NoLocalResourceInfo {
                name: name.clone(),
                target: target.dupe(),
            })?;
        resources.push(Resource { name, target, info });
    }
    Ok(resources)
}

/// The artifacts that the setup commands of `resources` name. They count as the server's: a
/// change to them may make a setup command new.
fn resource_inputs(resources: &[Resource]) -> buck2_error::Result<Vec<ArtifactGroup>> {
    let mut visitor = SimpleCommandLineArtifactVisitor::new();
    for resource in resources {
        resource
            .setup_command_line()
            .visit_artifacts(&mut visitor)?;
    }
    Ok(visitor.inputs.into_iter().collect())
}

/// The `Policy` of a `DevInfo`. `resource_inputs` are what the setup commands of the local
/// resources name, which belong to the server's slot.
fn dev_policy(
    info: DevView<'_>,
    resource_inputs: Vec<ArtifactGroup>,
) -> buck2_error::Result<Policy> {
    let outputs = info.outputs()?;
    if outputs.len() + 1 > MAX_SLOTS {
        return Err(DevError::TooManySlots("outputs and the server", outputs.len() + 1).into());
    }
    let scopes: Vec<(String, Vec<String>)> = info.scopes().into_iter().collect();
    if scopes.len() > MAX_SLOTS {
        return Err(DevError::TooManySlots("scopes", scopes.len()).into());
    }
    let sidecars = info.sidecars()?;
    let mut visitor = SimpleCommandLineArtifactVisitor::new();
    info.server_command_line().visit_artifacts(&mut visitor)?;
    let mut slots = vec![Slot {
        what: "`DevInfo.server`".to_owned(),
        inputs: visitor.inputs.into_iter().chain(resource_inputs).collect(),
    }];
    let mut names = Vec::new();
    for (name, output) in outputs.iter() {
        // What a sidecar, or an associated artifact, is built from counts as what the output is
        // built from.
        let mut inputs = output.groups();
        for (_, sidecar) in sidecars.get(name).into_iter().flatten() {
            inputs.extend(sidecar.groups());
        }
        slots.push(Slot {
            what: format!("Output `{name}`"),
            inputs,
        });
        names.push(name.clone());
    }
    Ok(Policy {
        names,
        slots,
        scopes,
    })
}

pub(crate) async fn dev_command(
    ctx: &dyn ServerCommandContextTrait,
    partial_result_dispatcher: PartialResultDispatcher<
        buck2_cli_proto::NewGenericPartialResultMessage,
    >,
    req: DevRequest,
) -> buck2_error::Result<DevResponse> {
    // A build stops when a file that it builds from changes. Subscribe before the command syncs
    // the file watcher, so that no change falls between what the build sees and what the
    // subscription reports. A change in between is reported but is no change to the build.
    let changes = match &req.action {
        DevAction::Build { .. } => ctx.subscribe_to_file_changes().await.ok(),
        DevAction::Server | DevAction::Watch(_) | DevAction::Release => None,
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
    type PartialResult = buck2_cli_proto::NewGenericPartialResultMessage;

    async fn command(
        &self,
        server_ctx: &dyn ServerCommandContextTrait,
        partial_result_dispatcher: PartialResultDispatcher<Self::PartialResult>,
        mut ctx: DiceTransaction,
    ) -> buck2_error::Result<Self::Response> {
        let changes = self
            .changes
            .lock()
            .ok()
            .and_then(|mut changes| changes.take());
        let partial_results = Mutex::new(partial_result_dispatcher);
        let response = dev(server_ctx, &mut ctx, &self.req, changes, &partial_results).await;
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
    partial_results: &Mutex<
        PartialResultDispatcher<buck2_cli_proto::NewGenericPartialResultMessage>,
    >,
) -> buck2_error::Result<DevResponse> {
    if let DevAction::Release = &req.action {
        // Needs no analysis: the target may not build any more.
        if let Some(resources) = session_held_if_any::<SessionResources>(&req.session) {
            resources.release_all().await?;
        }
        return Ok(DevResponse::Released);
    }
    let global_cfg_options =
        global_cfg_options_from_client_context(&req.target_cfg, server_ctx, &mut ctx.ctx())
            .await?;
    let subject = DevSubject::of_request(server_ctx, ctx, req, &global_cfg_options).await?;
    let providers = subject.providers(&mut ctx.ctx()).await?;
    // Held across awaits, so this needs the owned form.
    let info = DevProvider::of(&providers, &subject)?;
    let outputs = info.view().outputs()?;
    match &req.action {
        DevAction::Server => {
            let (server, digest) = build_command(
                &mut ctx.ctx(),
                || info.view().server_command_line(),
                "Failed to build the `buck2 dev` server",
            )
            .await?;
            let resources = dev_resources(&mut ctx.ctx(), &info).await?;
            let resources = if resources.is_empty()
                && session_held_if_any::<SessionResources>(&req.session).is_none()
            {
                ResourcesUpdate::default()
            } else {
                let mut wanted = Vec::with_capacity(resources.len());
                for resource in resources {
                    let what = format!(
                        "Failed to build the setup command of local resource `{}` (`{}`)",
                        resource.name, resource.target
                    );
                    let (argv, digest) =
                        build_command(&mut ctx.ctx(), || resource.setup_command_line(), &what)
                            .await?;
                    let env = resource
                        .info
                        .as_ref()
                        .value()
                        .as_ref()
                        .env_var_mapping()
                        .into_iter()
                        .collect();
                    wanted.push(WantedResource {
                        name: resource.name,
                        target: resource.target,
                        info: resource.info,
                        key: SetupKey { argv, digest, env },
                    });
                }
                let held = session_held(&req.session, SessionResources::default)
                    .ok_or_else(|| DevError::NoSession(req.session.clone()))?;
                held.update(&mut ctx.ctx(), server_ctx.cancellation_context(), wanted)
                    .await?
            };
            Ok(DevResponse::Server(DevServer {
                project_root: server_ctx.project_root().root().to_string(),
                server,
                digest,
                outputs: outputs.keys().cloned().collect(),
                scopes: info.view().scopes().into_keys().collect(),
                handlers: info
                    .view()
                    .handlers()
                    .into_iter()
                    .map(|(name, _)| name)
                    .collect(),
                interactive: info.view().interactive(),
                env: resources.env,
                resources: resources.running,
                resources_exited: resources.exited,
                resource_errors: resources.errors,
            }))
        }
        DevAction::Watch(request) => {
            let resources = dev_resources(&mut ctx.ctx(), &info).await?;
            let policy = dev_policy(info.view(), resource_inputs(&resources)?)?;
            let (mut watched, claims) =
                watch_files(&mut ctx.ctx(), &subject, &policy, request).await?;
            let mut interests = vec![claims.interest.dupe()];
            // A handler that fails may be fixed by a change outside what anything is built
            // from, such as to the file that it reads.
            let mut all_known = true;
            let handlers: Vec<buck2_error::Result<DevSubject>> = {
                let mut handlers = Vec::with_capacity(request.dynamic.len());
                for dynamic in &request.dynamic {
                    handlers.push(
                        dynamic_subject(
                            server_ctx,
                            ctx,
                            &subject,
                            &info,
                            &global_cfg_options,
                            &dynamic.call,
                        )
                        .await,
                    );
                }
                handlers
            };
            let subject = &subject;
            let scopes = &policy.scopes;
            let results = ctx
                .ctx()
                .compute_join(
                    request.dynamic.iter().zip(handlers),
                    async |dice, (dynamic, handler)| {
                        let result = async {
                            let claims = dynamic_claims(dice, subject, handler?).await?;
                            let policy = dynamic_policy(&dynamic.call.name, scopes.clone());
                            let changes = claims_changes(
                                dice,
                                &claims,
                                &policy,
                                &request.paths,
                                dynamic.previous.as_ref(),
                                request.missed,
                            )
                            .await?;
                            buck2_error::Ok((changes, claims))
                        }
                        .await;
                        (dynamic, result)
                    },
                )
                .await;
            for (dynamic, result) in results {
                let name = &dynamic.call.name;
                match result {
                    Ok((changes, claims)) => {
                        interests.push(claims.interest.dupe());
                        watched.outputs.outputs.extend(changes.changes.outputs);
                        for message in &claims.incomplete {
                            if !watched.incomplete.contains(message) {
                                watched.incomplete.push(message.clone());
                            }
                        }
                        watched.dynamic.insert(
                            name.clone(),
                            DevDynamicWatched {
                                state: Some(changes.state),
                                graph_changed: changes.graph_changed,
                            },
                        );
                    }
                    Err(_) => {
                        // What the output is built from is not known, so any change may be to
                        // it. The server hears why when it asks for it.
                        all_known = false;
                        let mut list: Vec<DevChange> = request
                            .paths
                            .iter()
                            .map(|path| DevChange {
                                path: Some(path.clone()),
                                scopes: Vec::new(),
                            })
                            .collect();
                        if request.missed {
                            list.push(DevChange {
                                path: Some(String::new()),
                                scopes: Vec::new(),
                            });
                        }
                        if list.is_empty() && dynamic.previous.is_some() {
                            // It failed since the last answer, after a change that names no
                            // file.
                            list.push(DevChange {
                                path: None,
                                scopes: Vec::new(),
                            });
                        }
                        if !list.is_empty() {
                            watched.outputs.outputs.insert(name.clone(), list);
                        }
                        watched.dynamic.insert(
                            name.clone(),
                            DevDynamicWatched {
                                state: None,
                                graph_changed: true,
                            },
                        );
                    }
                }
            }
            // From here on the session's file change subscription drops the changes that cannot
            // matter to these claims. `buck2 dev` asks again after every change that is let
            // through, and so after every change to the claims.
            if all_known {
                set_interest(
                    &req.session,
                    Arc::new(FileInterest::union(interests.iter().map(|i| &**i))),
                );
            } else {
                clear_interest(&req.session);
            }
            // A local resource that exited is set up again by the analysis that the client runs
            // when the server may be new.
            if let Some(resources) = session_held_if_any::<SessionResources>(&req.session) {
                if resources.any_exited().await {
                    watched.outputs.server = true;
                }
            }
            Ok(DevResponse::Watch(watched))
        }
        DevAction::Release => Err(internal_error!("`Release` is handled above")),
        DevAction::Build {
            outputs: names,
            streaming,
            dynamic,
        } => {
            let sidecars = info.view().sidecars()?;
            let mut wanted: Vec<Wanted> = Vec::with_capacity(names.len());
            // The dynamic outputs that cannot be built, with why: they fail on their own.
            let mut unbuildable: BTreeMap<String, String> = BTreeMap::new();
            // The handler of each dynamic output to build.
            let mut handlers: Vec<(String, DevSubject)> = Vec::new();
            for name in names {
                if let Some(call) = dynamic.iter().find(|call| call.name == *name) {
                    match resolve_dynamic(
                        server_ctx,
                        ctx,
                        &subject,
                        &info,
                        &outputs,
                        &global_cfg_options,
                        call,
                    )
                    .await
                    {
                        Ok((handler, output)) => {
                            wanted.push(Wanted {
                                name: name.clone(),
                                output,
                                sidecars: Vec::new(),
                            });
                            handlers.push((name.clone(), handler));
                        }
                        Err(e) => {
                            unbuildable.insert(name.clone(), output_error(&e));
                        }
                    }
                    continue;
                }
                let output = outputs.get(name).ok_or_else(|| DevError::UnknownOutput {
                    subject: subject.dupe(),
                    name: name.clone(),
                })?;
                wanted.push(Wanted {
                    name: name.clone(),
                    output: output.clone(),
                    sidecars: sidecars.get(name).cloned().unwrap_or_default(),
                });
            }
            if wanted.is_empty() {
                return Ok(DevResponse::Build(DevBuilt {
                    outputs: BTreeMap::new(),
                    failed: unbuildable,
                    dynamic: BTreeMap::new(),
                }));
            }
            // What each dynamic output is built from, which a `Watch` compares with next. The
            // session's file change subscription lets through the changes to it from now on.
            let subject = &subject;
            let found = ctx
                .ctx()
                .compute_join(handlers, async |dice, (name, handler)| {
                    (name, dynamic_claims(dice, subject, handler).await)
                })
                .await;
            let mut states = BTreeMap::new();
            let mut dynamic_builds: Vec<(String, Arc<DevClaims>, AtomicU64)> = Vec::new();
            for (name, claims) in found {
                // An output whose claims cannot be found does not stop the build.
                if let Ok(claims) = claims {
                    add_interest(&req.session, &claims.interest);
                    states.insert(name.clone(), claims.state());
                    dynamic_builds.push((name, claims, AtomicU64::new(slot_bit(1))));
                }
            }
            let info = &info;
            let argv = move |name: &str, artifact_fs: &ArtifactFs, ensured: &Ensured| {
                let command = info
                    .view()
                    .output_command_line(name)
                    .ok_or_else(|| internal_error!("Output `{name}` is not a command line"))?;
                command_argv(artifact_fs, command, ensured)
            };
            let argv: Argv = &argv;
            // The slots of the outputs that the build has yet to finish. A change to a file of
            // one of them stops it. Only the outputs' slots matter here.
            let all = dev_policy(info.view(), Vec::new())?.names;
            let builds = AtomicU64::new(
                all.iter()
                    .enumerate()
                    .filter(|(_, name)| {
                        names.contains(name) && !dynamic.iter().any(|call| call.name == **name)
                    })
                    .fold(0, |mask, (output, _)| mask | slot_bit(output + 1)),
            );
            let builds = &builds;
            let all = &all;
            let dynamic_builds = &dynamic_builds;
            let emit = move |built: DevBuilt| -> buck2_error::Result<()> {
                for name in built.outputs.keys().chain(built.failed.keys()) {
                    if let Some((_, _, building)) =
                        dynamic_builds.iter().find(|(dynamic, _, _)| dynamic == name)
                    {
                        building.store(0, Ordering::SeqCst);
                    } else if let Some(output) = all.iter().position(|n| n == name) {
                        builds.fetch_and(!slot_bit(output + 1), Ordering::SeqCst);
                    }
                }
                let json = serde_json::to_string(&NewGenericPartialResult::Dev(built))
                    .buck_error_context("Could not serialize `NewGenericPartialResult`")?;
                if let Ok(mut partial_results) = partial_results.lock() {
                    partial_results.emit(buck2_cli_proto::NewGenericPartialResultMessage {
                        new_generic_partial_result: json,
                    });
                }
                Ok(())
            };
            let stream: Option<Stream> = streaming.then_some(&emit);
            if let Some(stream) = stream {
                // A streaming build sends each output that failed on its own.
                for (name, error) in std::mem::take(&mut unbuildable) {
                    stream(DevBuilt {
                        outputs: BTreeMap::new(),
                        failed: BTreeMap::from([(name, error)]),
                        dynamic: BTreeMap::new(),
                    })?;
                }
            }
            let finish = |built: buck2_error::Result<DevBuilt>| {
                let mut built = built?;
                built.failed.extend(unbuildable);
                built.dynamic = states;
                buck2_error::Ok(DevResponse::Build(built))
            };
            let Some(changes) = changes else {
                return finish(
                    build(
                        server_ctx.project_root(),
                        &mut ctx.ctx(),
                        wanted,
                        argv,
                        stream,
                    )
                    .await,
                );
            };
            let claims = dev_claims(&mut ctx.ctx(), subject).await?;
            let mut watched: Vec<(&DevClaims, &AtomicU64)> = vec![(&*claims, builds)];
            watched.extend(
                dynamic_builds
                    .iter()
                    .map(|(_, claims, building)| (&**claims, building)),
            );
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
            let watched = &watched;
            let mut dice = ctx.ctx();
            let (built, interrupted) = dice
                .compute2(
                    async move |dice| {
                        tokio::select! {
                            built = build(project_root, dice, wanted, argv, stream) => {
                                let _ignored = built_tx.send(());
                                Some(built)
                            }
                            // Dropping the build cancels it.
                            _ = changed_rx => None,
                        }
                    },
                    async move |dice| {
                        tokio::select! {
                            interrupted = changed_files(dice, changes, watched, io) => {
                                let _ignored = changed_tx.send(());
                                Some(interrupted)
                            }
                            _ = built_rx => None,
                        }
                    },
                )
                .await;
            match (built, interrupted) {
                (Some(built), _) => finish(built),
                (None, Some(interrupted)) => Ok(DevResponse::Interrupted(interrupted)),
                (None, None) => Err(internal_error!(
                    "A `buck2 dev` build stopped without a file change"
                )),
            }
        }
    }
}

/// Build what a command line names, and return the command line, with absolute paths, and a
/// digest of the contents of what it names, hidden artifacts too, which is the same in another
/// daemon. `line` gives the command line; it is called once before the
/// build and once after.
async fn build_command<'a>(
    dice: &mut DiceComputations<'_>,
    line: impl Send + Sync + Fn() -> &'a dyn CommandLineArgLike<'a>,
    what: &str,
) -> buck2_error::Result<(Vec<String>, String)> {
    let artifact_fs = dice.get_artifact_fs().await?;
    let inputs = {
        let mut visitor = SimpleCommandLineArtifactVisitor::new();
        line().visit_artifacts(&mut visitor)?;
        visitor.inputs
    };
    let ensured = materialize_groups(dice, inputs.into_iter().collect())
        .await
        .with_buck_error_context(|| what.to_owned())?;
    let digest = contents_digest(contents(&artifact_fs, &ensured)?);
    let args = command_argv(&artifact_fs, line(), &ensured)?;
    Ok((args, digest))
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
/// which did. A file saved without a change, and one that nothing builds from, are no change.
/// `claims` has the claims of the target, and of each dynamic output that the build builds, each
/// with the slots of the outputs that the build has yet to finish.
async fn changed_files(
    dice: &mut DiceComputations<'_>,
    mut changes: BoxStream<'static, FileChangeBatch>,
    claims: &[(&DevClaims, &AtomicU64)],
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
            if !claims.iter().any(|(claims, _)| claims.may_claim(&path)) {
                continue;
            }
            // A path that cannot be looked at counts as changed.
            if builds_from_changed(dice, claims, &io, &path)
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

/// Whether what the build has yet to finish, the slots of each of `claims`, is built from
/// `path`, and the file differs from what this command saw.
async fn builds_from_changed(
    dice: &mut DiceComputations<'_>,
    claims: &[(&DevClaims, &AtomicU64)],
    io: &Arc<dyn IoProvider>,
    path: &ProjectRelativePath,
) -> buck2_error::Result<bool> {
    let cell_resolver = dice.get_cell_resolver().await?;
    let mut relevant = false;
    for (claims, builds) in claims {
        let builds = builds.load(Ordering::SeqCst);
        if builds == 0 || !claims.may_claim(path) {
            continue;
        }
        relevant = match claims.classify(dice, cell_resolver, path).await? {
            Claimed::Source { may, .. } => may & builds != 0,
            Claimed::BuildFile | Claimed::Bzl => true,
            Claimed::Nothing => false,
        };
        if relevant {
            break;
        }
    }
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

/// An output to build.
struct Wanted {
    name: String,
    output: DevOutputSpec,
    /// The sidecars of the output, by name. They are built with it.
    sidecars: Vec<(String, DevArtifact)>,
}

/// Resolves the command line of an output, by its name, once what it names is built.
type Argv<'a> =
    &'a (dyn Fn(&str, &ArtifactFs, &Ensured) -> buck2_error::Result<Vec<String>> + Sync);

/// Built artifacts, as `materialize_groups` gives them.
type Ensured = Vec<(ArtifactGroup, ArtifactGroupValues)>;

/// `command` as the server gets it, with the absolute paths of the artifacts in `ensured`.
fn command_argv<'v>(
    artifact_fs: &ArtifactFs,
    command: &'v dyn CommandLineArgLike<'v>,
    ensured: &Ensured,
) -> buck2_error::Result<Vec<String>> {
    let path_separator = if cfg!(windows) {
        PathSeparatorKind::Windows
    } else {
        PathSeparatorKind::Unix
    };
    let executor_fs = ExecutorFs::new(artifact_fs, path_separator);
    let mut args = Vec::<String>::new();
    let mapper = ArtifactPathMapperImpl::from(ensured);
    let mut builder =
        CommandLineBuilder::new_with_options(&mut args, &mapper, &executor_fs, true, None);
    command.add_to_command_line(&mut builder)?;
    Ok(args)
}

/// Build and materialise each of `groups`, all or none.
async fn materialize_groups(
    dice: &mut DiceComputations<'_>,
    groups: Vec<ArtifactGroup>,
) -> buck2_error::Result<Ensured> {
    dice.try_compute_join(groups, async |dice, group| {
        materialize_and_upload_artifact_group(
            dice,
            &group,
            MaterializationAndUploadContext::materialize(),
            &dice
                .per_transaction_data()
                .get_materialization_queue_tracker(),
        )
        .await
        .map(|values| (group, values))
    })
    .await
}

/// The paths of the built artifacts of `groups`, relative to the project root, with what is in
/// each, for `contents_digest`.
fn contents(
    artifact_fs: &ArtifactFs,
    groups: &[(ArtifactGroup, ArtifactGroupValues)],
) -> buck2_error::Result<Vec<(String, String)>> {
    let mut contents = Vec::new();
    for (_, values) in groups {
        for (artifact, value) in values.iter() {
            let path =
                artifact.resolve_path(artifact_fs, Some(&value.content_based_path_hash()))?;
            contents.push((path.to_string(), value_digest(value)));
        }
    }
    Ok(contents)
}

/// Hands on the result of one output of a streaming build.
type Stream<'a> = &'a (dyn Fn(DevBuilt) -> buck2_error::Result<()> + Sync);

/// Build each output on its own: one that fails does not keep the others from the server. An
/// output fails if one of its sidecars, or an artifact associated with it or with a sidecar,
/// does. With `stream`, the result of each output but the last to finish goes to `stream` as
/// soon as it is known, and the result has only the last.
async fn build(
    project_root: &ProjectRoot,
    dice: &mut DiceComputations<'_>,
    wanted: Vec<Wanted>,
    argv: Argv<'_>,
    stream: Option<Stream<'_>>,
) -> buck2_error::Result<DevBuilt> {
    let artifact_fs = dice.get_artifact_fs().await?;
    let artifact_fs = &artifact_fs;
    // The outputs that have yet to finish.
    let remaining = AtomicUsize::new(wanted.len());
    let remaining = &remaining;
    let results = dice
        .compute_join(wanted, async |dice, wanted| {
            let Wanted {
                name,
                output,
                sidecars,
            } = wanted;
            let command = matches!(output, DevOutputSpec::Command(_));
            // The output itself has no name here.
            let mut artifacts = vec![(None, output.groups())];
            artifacts.extend(
                sidecars
                    .into_iter()
                    .map(|(sidecar, artifact)| (Some(sidecar), artifact.groups())),
            );
            let values = dice
                .try_compute_join(artifacts, async |dice, (sidecar, groups)| {
                    let groups = materialize_groups(dice, groups).await?;
                    buck2_error::Ok((sidecar, groups))
                })
                .await;
            let result = match values {
                Ok(values) => {
                    output_result(project_root, artifact_fs, argv, &name, command, values)?
                }
                Err(e) => Err(output_error(&e)),
            };
            if let Some(stream) = stream {
                if remaining.fetch_sub(1, Ordering::SeqCst) > 1 {
                    let mut built = DevBuilt {
                        outputs: BTreeMap::new(),
                        failed: BTreeMap::new(),
                        dynamic: BTreeMap::new(),
                    };
                    match result {
                        Ok(output) => {
                            built.outputs.insert(name, output);
                        }
                        Err(error) => {
                            built.failed.insert(name, error);
                        }
                    }
                    stream(built)?;
                    return buck2_error::Ok(None);
                }
            }
            buck2_error::Ok(Some((name, result)))
        })
        .await;
    let mut outputs = BTreeMap::new();
    let mut failed = BTreeMap::new();
    for result in results {
        match result? {
            Some((name, Ok(output))) => {
                outputs.insert(name, output);
            }
            Some((name, Err(error))) => {
                failed.insert(name, error);
            }
            None => {}
        }
    }
    Ok(DevBuilt {
        outputs,
        failed,
        dynamic: BTreeMap::new(),
    })
}

/// What the server gets of an output that built, from its artifacts and those of its sidecars,
/// the output's first. `Err` says why it cannot have it.
fn output_result(
    project_root: &ProjectRoot,
    artifact_fs: &ArtifactFs,
    argv: Argv<'_>,
    name: &str,
    command: bool,
    mut values: Vec<(Option<String>, Ensured)>,
) -> buck2_error::Result<Result<DevBuiltOutput, String>> {
    let mut output = None;
    if command {
        // The output comes first. What its command line names is all that it is.
        let (_, groups) = values.remove(0);
        match argv(name, artifact_fs, &groups) {
            Ok(argv) => {
                let digest = contents_digest(contents(artifact_fs, &groups)?);
                output = Some((None, Some(argv), Some(digest)));
            }
            Err(e) => return Ok(Err(output_error(&e))),
        }
    }
    let mut sidecars = BTreeMap::new();
    for (sidecar, groups) in values {
        // The artifact comes first, and then those associated with it.
        let (artifact, value) = groups
            .first()
            .and_then(|(_, values)| values.iter().next())
            .ok_or_else(|| DevError::EmptyOutput(name.to_owned()))?;
        let path = artifact.resolve_path(artifact_fs, Some(&value.content_based_path_hash()))?;
        let path = project_root.resolve(&path).to_string();
        let digest = if groups.len() > 1 {
            // A new associated artifact is a new output, also with the same artifact.
            Some(contents_digest(contents(artifact_fs, &groups)?))
        } else {
            value.digest().map(|d| d.to_string())
        };
        match sidecar {
            Some(sidecar) => {
                sidecars.insert(sidecar, DevBuiltSidecar { path, digest });
            }
            None => output = Some((Some(path), None, digest)),
        }
    }
    let (path, argv, digest) = output.ok_or_else(|| DevError::EmptyOutput(name.to_owned()))?;
    Ok(Ok(DevBuiltOutput {
        path,
        argv,
        digest,
        sidecars,
    }))
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

/// What the slots claim, before it is made small enough to keep.
///
/// Finding it walks the targets of the graph, not their source files and not the action graph:
/// a slot claims the targets that its `inputs` are built from, with all their source files.
/// Which file that is comes from the targets when a file changes; see `DevClaims::classify`.
/// Only the actions of the targets that own the `inputs` are looked at, so that a target that
/// builds an input and other things does not give the slot the source files of the other
/// things.
struct GraphClaims {
    /// The targets of the graph.
    nodes: Vec<ConfiguredTargetNode>,
    /// The source files that slots claim by name: those that the actions of the targets that
    /// own their `inputs` use, and those that claimed symlinks point at.
    files: BuckMutMap<ProjectRelativePathBuf, (u64, CellPath)>,
    /// The slots that claim the own source files of each target.
    with_own_files: BuckMutMap<ConfiguredTargetLabel, u64>,
    /// The scopes that each target is in. Targets in no scope are not here.
    scopes: BuckMutMap<ConfiguredTargetLabel, u64>,
    /// The targets whose digests say whether the targets of the slots changed, in groups: by
    /// slot, and by the scopes that they are in.
    groups: BTreeMap<(usize, u64), Vec<ConfiguredTargetNode>>,
    /// How many targets a slot has.
    claimed_targets: usize,
    /// The directory of each package of the graph.
    package_dirs: BTreeSet<ProjectRelativePathBuf>,
    /// Each build file of the graph.
    build_files: BTreeMap<ProjectRelativePathBuf, CellPath>,
    /// The `.bzl` files that the graph loads.
    bzl_files: BTreeMap<ProjectRelativePathBuf, CellPath>,
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

/// Walk the configured dependency graph of the subject. Give each target to the slots that
/// claim it.
///
/// The graph of a target is the target and its dependencies. A bxl function is no target, so its
/// graph is made of the targets that the server and the outputs are built from, and their
/// dependencies.
///
/// This reads the contents of no source file, so that a `DevClaims` made from it stays valid
/// when one changes. The target patterns of the scopes are relative to `cell`.
async fn graph_claims(
    dice: &mut DiceComputations<'_>,
    subject: &DevSubject,
    policy: &Policy,
    cell: CellName,
) -> buck2_error::Result<GraphClaims> {
    let slots = &policy.slots;
    let mut timer = Timer::new();
    let cell_resolver = dice.get_cell_resolver().await?;
    let cell_alias_resolver = dice.get_cell_alias_resolver(cell).await?;

    let scope_patterns: Vec<Vec<ParsedPattern<TargetPatternExtra>>> = policy
        .scopes
        .iter()
        .map(|(_, targets)| {
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
        .collect::<buck2_error::Result<_>>()?;

    let mut walks = Vec::with_capacity(slots.len());
    for spec in slots {
        walks.push(if spec.inputs.is_empty() {
            InputsWalk::default()
        } else {
            walk_inputs(dice, &spec.inputs).await
        });
    }
    timer.step("the actions of the targets that own the inputs of each slot");

    let roots: Vec<ConfiguredTargetLabel> = match subject {
        DevSubject::Target(label) => vec![label.target().dupe()],
        DevSubject::Bxl { .. } => walks
            .iter()
            .flat_map(|walk| {
                walk.owners
                    .iter()
                    .chain(walk.beyond.iter())
                    .chain(walk.failed.iter().filter_map(|(owner, _)| owner.as_ref()))
            })
            .map(|label| label.dupe())
            .collect::<BuckMutInsertionSet<_>>()
            .into_iter()
            .collect(),
    };
    let roots = dice
        .try_compute_join(roots, async |dice, label| {
            dice.get_configured_target_node(&label)
                .await
                .require_compatible()
        })
        .await?;
    let nodes = dependency_closure(roots.into_iter().map(|node| node.dupe()));
    let by_label: BuckMutMap<ConfiguredTargetLabel, ConfiguredTargetNode> = nodes
        .iter()
        .map(|node| (node.label().dupe(), node.dupe()))
        .collect();
    timer.step(&format!("the {} targets of the graph", nodes.len()));

    // The slots that claim each target, and each source file that slots claim by name. Of the
    // targets that own the `inputs` of a slot, it claims only the source files that their
    // actions use. What those actions take from other targets gives it those targets and their
    // dependencies, with their own source files. The targets of a slot say which slots a build
    // file change changed.
    let mut files: BuckMutMap<ProjectRelativePathBuf, (u64, CellPath)> = BuckMutMap::default();
    let mut targets: BuckMutMap<ConfiguredTargetLabel, u64> = BuckMutMap::default();
    let mut with_own_files: BuckMutMap<ConfiguredTargetLabel, u64> = BuckMutMap::default();
    let mut scopes: BuckMutMap<ConfiguredTargetLabel, u64> = BuckMutMap::default();
    for (scope, patterns) in scope_patterns.iter().enumerate() {
        for node in &nodes {
            if patterns
                .iter()
                .any(|p| p.matches(node.label().unconfigured()))
            {
                claim(&mut scopes, node.label().dupe(), scope);
            }
        }
    }
    let mut incomplete = Vec::new();
    for (set, (spec, walk)) in slots.iter().zip(walks).enumerate() {
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
    timer.step("the targets of each slot");

    let mut groups: BTreeMap<(usize, u64), Vec<ConfiguredTargetNode>> = BTreeMap::new();
    let mut claimed_targets = 0;
    let mut build_files: BTreeMap<ProjectRelativePathBuf, CellPath> = BTreeMap::new();
    let mut packages = BTreeSet::new();
    let mut package_dirs = BTreeSet::new();
    for node in &nodes {
        let has = targets.get(node.label()).copied().unwrap_or(0);
        if has != 0 {
            claimed_targets += 1;
        }
        let in_scopes = scopes.get(node.label()).copied().unwrap_or(0);
        for slot in 0..slots.len() {
            if has & slot_bit(slot) == 0 {
                continue;
            }
            groups
                .entry((slot, in_scopes))
                .or_default()
                .push(node.dupe());
        }
        if packages.insert(node.label().pkg()) {
            if let Some(dir) = project_path(cell_resolver, node.label().pkg().as_cell_path())? {
                package_dirs.insert(dir);
            }
        }
        let buildfile = node.buildfile_path().path();
        if let Some(path) = project_path(cell_resolver, buildfile.dupe())? {
            build_files.entry(path).or_insert(buildfile);
        }
    }
    timer.step(&format!("the {} packages of the graph", packages.len()));

    let mut claims = GraphClaims {
        nodes,
        files,
        with_own_files,
        scopes,
        groups,
        claimed_targets,
        package_dirs,
        build_files,
        bzl_files: BTreeMap::new(),
        incomplete,
    };
    claim_symlink_targets(dice, cell_resolver, &packages, &mut claims).await?;
    timer.step("the symlinks in the packages");

    let mut imports: Vec<ImportPath> = Vec::new();
    for package in packages {
        imports.extend(
            dice.get_interpreter_results(package)
                .await?
                .imports()
                .iter()
                .cloned(),
        );
    }
    if let DevSubject::Bxl { file, .. } = subject {
        // The function's file says what the outputs are, as a `.bzl` file of a rule does.
        if let Some(path) = project_path(cell_resolver, file.dupe())? {
            claims.bzl_files.insert(path, file.dupe());
        }
        let file = BxlFilePath::new(file.dupe())?;
        imports.extend(
            dice.get_loaded_module(StarlarkModulePath::BxlFile(&file))
                .await?
                .imports()
                .cloned(),
        );
    }
    claims
        .bzl_files
        .extend(loaded_files(dice, cell_resolver, imports).await?);
    // A build file changes the graph, even when a target also uses it as a source file.
    for path in claims.build_files.keys() {
        claims.files.remove(path);
    }
    timer.step("the `.bzl` files that the packages load");
    Ok(claims)
}

/// A source file that is a symlink is built from the file that it points at, which the slot
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
    // Those that a slot claims by name.
    for (path, (set, cell_path)) in &claims.files {
        if symlinks.is_behind(dice, cell_resolver, path).await? {
            links.push((*set, cell_path.dupe()));
        }
    }
    // Those of the targets whose own source files a slot claims: the symlinks in a package are
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
/// What the slots claim depends on this, and not on the listing: a file that is added to the
/// directory, or removed from it, then leaves the claims as they are.
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
        /// The scopes that the change is in: those of the targets that have the file as an own
        /// source file.
        scopes: u64,
    },
    /// A build file of the graph.
    BuildFile,
    /// A `.bzl` file that the graph loads.
    Bzl,
    Nothing,
}

/// What the slots claim, small enough to keep: no file is here by name.
///
/// A slot claims most files as the source files of its targets, and the targets already have
/// those, so this only says which slot has which target. To find the slots of a path,
/// `classify` looks at the targets of the packages that the path is in, as the `owner()` query
/// does. Only the few files that a slot claims by name are kept, as hashes of their paths:
/// those that the actions of the targets that own its `inputs` use, and those that claimed
/// symlinks point at.
///
/// This depends on the graph and not on the contents of source files, so DICE keeps it while
/// only those change, and makes it again when a target, a build file or a `.bzl` file does.
/// The exceptions are slots whose `inputs` are made by dynamic actions of the targets that own
/// them, which depend on what those read.
#[derive(Allocative, Debug, PartialEq, Eq)]
struct DevClaims {
    /// For each target of the graph, by the hash of its unconfigured label: the slots that
    /// claim its own source files, and the scopes that it is in. Sorted.
    targets: Vec<(u64, u64, u64)>,
    /// The source files that slots claim by name, by the hash of their project-relative path,
    /// with the slots. Sorted.
    files: Vec<(u64, u64)>,
    /// The packages of the graph, and the files that matter outside them: what the session's
    /// file change subscription needs to drop the changes that cannot matter.
    interest: Arc<FileInterest>,
    /// The build files of the graph. Sorted.
    build_files: Vec<u64>,
    /// The `.bzl` files that the graph loads. Sorted.
    bzl_files: Vec<u64>,
    /// Each group of targets, by slot and scopes, with a digest of them; see
    /// `GraphClaims::groups`. Sorted.
    targets_digests: Vec<(usize, u64, String)>,
    /// How many targets a slot has.
    claimed_targets: usize,
    /// The `.bzl` files that the graph loads, and their contents.
    bzl_digest: String,
    /// The build files of the graph and the `.bzl` files, and their contents.
    graph_digest: String,
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
            with_own_files,
            scopes,
            groups,
            claimed_targets,
            package_dirs,
            build_files,
            bzl_files,
            incomplete,
        } = claims;

        let mut by_label: BTreeMap<u64, (u64, u64)> = BTreeMap::new();
        for (label, slots) in &with_own_files {
            by_label
                .entry(stable_hash(&label.unconfigured().to_string()))
                .or_default()
                .0 |= *slots;
        }
        for (label, in_scopes) in &scopes {
            by_label
                .entry(stable_hash(&label.unconfigured().to_string()))
                .or_default()
                .1 |= *in_scopes;
        }

        let mut by_path: BTreeMap<u64, u64> = BTreeMap::new();
        for (path, (slots, _)) in &files {
            *by_path.entry(stable_hash(path.as_str())).or_default() |= *slots;
        }
        let other_files: Vec<(u64, u64)> = by_path.into_iter().collect();

        // The contents of the files that say what the graph is.
        let mut graph_paths: Vec<CellPath> = build_files.values().map(|path| path.dupe()).collect();
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
            .map(|(path, cell_path)| (path, digest_of(cell_path)))
            .collect();
        graph_files.extend(
            bzl_files
                .iter()
                .map(|(path, cell_path)| (path, digest_of(cell_path))),
        );
        let graph_digest = files_digest(&mut graph_files);
        timer.step("the contents of the build files and `.bzl` files");

        let mut build_file_hashes: Vec<u64> = build_files
            .keys()
            .map(|path| stable_hash(path.as_str()))
            .collect();
        build_file_hashes.sort_unstable();
        let mut bzl_hashes: Vec<u64> = bzl_files
            .keys()
            .map(|path| stable_hash(path.as_str()))
            .collect();
        bzl_hashes.sort_unstable();

        let targets_digests: Vec<(usize, u64, String)> = groups
            .iter()
            .map(|((slot, in_scopes), nodes)| {
                (*slot, *in_scopes, targets_digest(nodes.iter().collect()))
            })
            .collect();
        timer.step("the digests of the targets of each slot");
        let interest = Arc::new(FileInterest::new(
            package_dirs.iter().map(|dir| &**dir),
            other_files
                .iter()
                .map(|(hash, _)| *hash)
                .chain(build_file_hashes.iter().copied())
                .chain(bzl_hashes.iter().copied()),
        ));
        Ok(DevClaims {
            targets: by_label
                .into_iter()
                .map(|(hash, (own, in_scopes))| (hash, own, in_scopes))
                .collect(),
            files: other_files,
            interest,
            build_files: build_file_hashes,
            bzl_files: bzl_hashes,
            targets_digests,
            claimed_targets,
            bzl_digest,
            graph_digest,
            incomplete,
        })
    }

    fn state(&self) -> DevWatchState {
        DevWatchState {
            targets_digests: self
                .targets_digests
                .iter()
                .map(|(slot, scopes, digest)| DevTargets {
                    slot: *slot,
                    scopes: *scopes,
                    digest: digest.clone(),
                })
                .collect(),
            bzl_digest: self.bzl_digest.clone(),
            graph_digest: self.graph_digest.clone(),
        }
    }

    /// Whether `path` may be something to the slots. This looks at nothing but the path, so it
    /// is cheap enough for every change in the project: `false` means that `classify` would say
    /// `Nothing`.
    fn may_claim(&self, path: &ProjectRelativePath) -> bool {
        self.interest.may_claim(path)
    }

    /// What `path` is to the slots, with the files as they are now.
    async fn classify(
        &self,
        dice: &mut DiceComputations<'_>,
        cell_resolver: &CellResolver,
        path: &ProjectRelativePath,
    ) -> buck2_error::Result<Claimed> {
        let hash = stable_hash(path.as_str());
        // A build file changes the graph, even when a target also uses it as a source file.
        if self.build_files.binary_search(&hash).is_ok() {
            return Ok(Claimed::BuildFile);
        }
        if self.bzl_files.binary_search(&hash).is_ok() {
            return Ok(Claimed::Bzl);
        }
        let mut may = match self.files.binary_search_by_key(&hash, |x| x.0) {
            Ok(i) => self.files[i].1,
            Err(_) => 0,
        };
        let mut scopes = 0;
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
                let (_, own, in_scopes) = self.targets[i];
                // Whether the target would add to the answer, if it has the file.
                let adds = own & !may != 0 || in_scopes & !scopes != 0;
                if adds && node.inputs().any(|input| input == cell_path) {
                    may |= own;
                    scopes |= in_scopes;
                }
            }
        }
        Ok(if may == 0 {
            Claimed::Nothing
        } else {
            Claimed::Source { may, scopes }
        })
    }
}

/// The `DevClaims` of a target.
#[derive(Clone, Dupe, Debug, Eq, Hash, PartialEq, Allocative, Pagable)]
#[pagable_typetag(dice::DiceKeyDyn)]
struct DevClaimsKey(DevSubject);

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
        let policy = dev_slots(ctx, &self.0).await?;
        let claims = graph_claims(ctx, &self.0, &policy, self.0.cell()).await?;
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
    subject: &DevSubject,
) -> buck2_error::Result<Arc<DevClaims>> {
    dice.compute(&DevClaimsKey(subject.dupe())).await?.dupe()
}

/// The `Policy` of the `DevInfo` of a target or a bxl function.
async fn dev_slots(
    dice: &mut DiceComputations<'_>,
    subject: &DevSubject,
) -> buck2_error::Result<Policy> {
    let providers = subject.providers(dice).await?;
    let info = DevProvider::of(&providers, subject)?;
    let resources = dev_resources(dice, &info).await?;
    dev_policy(info.view(), resource_inputs(&resources)?)
}

/// The handler of a dynamic output, as a bxl function with the output's arguments.
async fn dynamic_subject(
    server_ctx: &dyn ServerCommandContextTrait,
    ctx: &DiceTransaction,
    subject: &DevSubject,
    info: &DevProvider,
    global_cfg_options: &GlobalCfgOptions,
    call: &DevDynamicCall,
) -> buck2_error::Result<DevSubject> {
    let label = info
        .view()
        .handlers()
        .into_iter()
        .find(|(name, _)| *name == call.handler)
        .map(|(_, label)| label)
        .ok_or_else(|| DevError::UnknownHandler {
            subject: subject.dupe(),
            name: call.handler.clone(),
        })?;
    DevSubject::bxl(server_ctx, ctx, &label, &call.args, global_cfg_options)
        .await
        .with_buck_error_context(|| format!("For handler `{}`", call.handler))
}

/// The handler of a dynamic output, and what to build for it.
async fn resolve_dynamic(
    server_ctx: &dyn ServerCommandContextTrait,
    ctx: &DiceTransaction,
    subject: &DevSubject,
    info: &DevProvider,
    outputs: &BuckMutInsertionMap<String, DevOutputSpec>,
    global_cfg_options: &GlobalCfgOptions,
    call: &DevDynamicCall,
) -> buck2_error::Result<(DevSubject, DevOutputSpec)> {
    if outputs.contains_key(&call.name) {
        return Err(DevError::DynamicNameTaken(call.name.clone()).into());
    }
    let handler =
        dynamic_subject(server_ctx, ctx, subject, info, global_cfg_options, call).await?;
    let output = handler_output(&mut ctx.ctx(), &handler).await?;
    Ok((handler, output))
}

/// The output of a handler: the one default output of the providers that it returns.
async fn handler_output(
    dice: &mut DiceComputations<'_>,
    handler: &DevSubject,
) -> buck2_error::Result<DevOutputSpec> {
    let providers = handler.providers(dice).await?;
    let what = match handler {
        DevSubject::Bxl { key, .. } => key.to_string(),
        DevSubject::Target(label) => label.to_string(),
    };
    Ok(DevOutputSpec::Artifact(DevArtifact::of_default_info(
        &providers, &what,
    )?))
}

/// The `Policy` of a dynamic output `name`: an empty slot for the server, and one for the
/// output. Only its names and scopes are used.
fn dynamic_policy(name: &str, scopes: Vec<(String, Vec<String>)>) -> Policy {
    Policy {
        names: vec![name.to_owned()],
        slots: vec![
            Slot {
                what: "`DevInfo.server`".to_owned(),
                inputs: Vec::new(),
            },
            Slot {
                what: format!("Dynamic output `{name}`"),
                inputs: Vec::new(),
            },
        ],
        scopes,
    }
}

/// The `DevClaims` of a dynamic output: what the output of `handler`, a bxl function with its
/// arguments, may be built from, with the scopes of the `DevInfo` of `main`. Its slot 1 is the
/// output, and slot 0 claims nothing.
///
/// The key does not name the output: outputs that call the same handler with the same arguments
/// share it.
#[derive(Clone, Dupe, Debug, Eq, Hash, PartialEq, Allocative, Pagable)]
#[pagable_typetag(dice::DiceKeyDyn)]
struct DevDynamicClaimsKey {
    main: DevSubject,
    handler: DevSubject,
}

impl std::fmt::Display for DevDynamicClaimsKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DevDynamicClaims({}, {})", self.main, self.handler)
    }
}

#[async_trait]
impl Key for DevDynamicClaimsKey {
    type Value = buck2_error::Result<Arc<DevClaims>>;

    async fn compute(
        &self,
        ctx: &mut DiceComputations,
        _cancellations: &CancellationContext,
    ) -> Self::Value {
        let providers = self.main.providers(ctx).await?;
        let info = DevProvider::of(&providers, &self.main)?;
        let scopes: Vec<(String, Vec<String>)> = info.view().scopes().into_iter().collect();
        let output = handler_output(ctx, &self.handler).await?;
        let policy = Policy {
            names: vec![String::new()],
            slots: vec![
                Slot {
                    what: "`DevInfo.server`".to_owned(),
                    inputs: Vec::new(),
                },
                Slot {
                    what: format!("The output of {}", self.handler),
                    inputs: output.groups(),
                },
            ],
            scopes,
        };
        let claims = graph_claims(ctx, &self.handler, &policy, self.main.cell()).await?;
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

async fn dynamic_claims(
    dice: &mut DiceComputations<'_>,
    main: &DevSubject,
    handler: DevSubject,
) -> buck2_error::Result<Arc<DevClaims>> {
    dice.compute(&DevDynamicClaimsKey {
        main: main.dupe(),
        handler,
    })
    .await?
    .dupe()
}

/// Say what the requested paths are to the server and to the outputs, and whose targets
/// changed since `request.previous`. Whether a file changed is not said here: a path that
/// `buck2 dev` asks about did.
async fn watch_files(
    dice: &mut DiceComputations<'_>,
    subject: &DevSubject,
    policy: &Policy,
    request: &DevWatchRequest,
) -> buck2_error::Result<(DevWatched, Arc<DevClaims>)> {
    let claims = dev_claims(dice, subject).await?;
    let changes = claims_changes(
        dice,
        &claims,
        policy,
        &request.paths,
        request.previous.as_ref(),
        request.missed && request.previous.is_some(),
    )
    .await?;
    let watched = DevWatched {
        state: changes.state,
        graph_changed: changes.graph_changed,
        incomplete: claims.incomplete.clone(),
        outputs: changes.changes,
        dynamic: BTreeMap::new(),
    };
    Ok((watched, claims))
}

/// What changes are to the slots of some claims.
struct ClaimsChanges {
    changes: DevOutputChanges,
    state: DevWatchState,
    /// A build file or a `.bzl` file of the graph, or a target, changed since `previous`.
    graph_changed: bool,
}

/// Say what `paths` are to the slots of `claims`, of which `policy` names the outputs, and whose
/// targets changed since `previous`. With `missed`, anything may have changed.
async fn claims_changes(
    dice: &mut DiceComputations<'_>,
    claims: &DevClaims,
    policy: &Policy,
    paths: &[String],
    previous: Option<&DevWatchState>,
    missed: bool,
) -> buck2_error::Result<ClaimsChanges> {
    let mut outputs = OutputChanges::new(policy);
    let cell_resolver = dice.get_cell_resolver().await?;

    for path in paths {
        let Ok(rel) = ProjectRelativePath::new(path) else {
            continue;
        };
        if claims.may_claim(rel) {
            let claimed = claims.classify(dice, cell_resolver, rel).await?;
            outputs.path(path, &claimed);
        }
    }

    let state = claims.state();
    // The groups of targets that changed; see `GraphClaims::groups`.
    let (changed_groups, every, graph_changed) = match previous {
        None => (Vec::new(), false, false),
        Some(previous) => {
            // A `.bzl` file may change what any rule does, so it changes every group.
            let every = previous.bzl_digest != state.bzl_digest;
            let old: BTreeMap<(usize, u64), &str> = previous
                .targets_digests
                .iter()
                .map(|group| ((group.slot, group.scopes), group.digest.as_str()))
                .collect();
            let new: BTreeMap<(usize, u64), &str> = state
                .targets_digests
                .iter()
                .map(|group| ((group.slot, group.scopes), group.digest.as_str()))
                .collect();
            // A group that is new, or gone, changed too.
            let changed: Vec<(usize, u64)> = old
                .keys()
                .chain(new.keys())
                .copied()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .filter(|key| every || old.get(key) != new.get(key))
                .collect();
            let targets_changed = previous.targets_digests != state.targets_digests;
            (
                changed,
                every,
                targets_changed || previous.graph_digest != state.graph_digest,
            )
        }
    };

    if missed {
        outputs.missed();
    }
    Ok(ClaimsChanges {
        changes: outputs.finish(
            &changed_groups,
            every,
            graph_changed,
            previous.is_some(),
            claims,
        ),
        state,
        graph_changed,
    })
}

/// Gathers what changes are to the server and to the outputs.
struct OutputChanges<'a> {
    policy: &'a Policy,
    changes: DevOutputChanges,
    /// The requested paths that are build files or `.bzl` files of the graph.
    graph_paths: Vec<String>,
}

impl<'a> OutputChanges<'a> {
    fn new(policy: &'a Policy) -> Self {
        OutputChanges {
            policy,
            changes: DevOutputChanges::default(),
            graph_paths: Vec::new(),
        }
    }

    fn add(&mut self, slot: usize, path: Option<String>, scopes: u64) {
        if slot == 0 {
            self.changes.server = true;
            return;
        }
        let Some(name) = self.policy.names.get(slot - 1) else {
            return;
        };
        self.changes
            .outputs
            .entry(name.clone())
            .or_default()
            .push(DevChange {
                path,
                scopes: self.policy.scope_names(scopes),
            });
    }

    /// Changes were missed, so anything may have changed: the server may be new, and every
    /// output may be built from what changed, in no scope.
    fn missed(&mut self) {
        for slot in 0..self.policy.slots.len() {
            self.add(slot, Some(String::new()), 0);
        }
    }

    /// The requested path `path` changed.
    fn path(&mut self, path: &str, claimed: &Claimed) {
        let (may, scopes) = match claimed {
            Claimed::Source { may, scopes } => (*may, *scopes),
            Claimed::BuildFile | Claimed::Bzl => {
                self.graph_paths.push(path.to_owned());
                return;
            }
            Claimed::Nothing => return,
        };
        for slot in 0..self.policy.slots.len() {
            if may & slot_bit(slot) != 0 {
                self.add(slot, Some(path.to_owned()), scopes);
            }
        }
    }

    /// A change to the graph for `slot`, in the scopes `scopes`: the requested paths that are
    /// build files or `.bzl` files of the graph.
    fn add_graph(&mut self, slot: usize, scopes: u64, graph_paths: &[String]) {
        if graph_paths.is_empty() {
            // The targets changed, but no requested path says why, as after a change to a
            // `.buckconfig` file.
            self.add(slot, None, scopes);
        }
        for path in graph_paths {
            self.add(slot, Some(path.clone()), scopes);
        }
    }

    /// The groups of targets `changed_groups` changed, each a slot and the scopes of its
    /// targets. With `every`, a `.bzl` file changed, which is in no scope, and which may change
    /// every output, also one that no target builds. Without `previous_known`, there was no
    /// earlier answer to compare with, and a build file or `.bzl` file of the graph among the
    /// requested paths may have changed any output.
    fn finish(
        mut self,
        changed_groups: &[(usize, u64)],
        every: bool,
        graph_changed: bool,
        previous_known: bool,
        claims: &DevClaims,
    ) -> DevOutputChanges {
        self.changes.server |= graph_changed;
        let graph_paths = std::mem::take(&mut self.graph_paths);
        for (slot, scopes) in changed_groups {
            let scopes = if every { 0 } else { *scopes };
            self.add_graph(*slot, scopes, &graph_paths);
        }
        if every || (!previous_known && !graph_paths.is_empty()) {
            for slot in 1..self.policy.slots.len() {
                self.add_graph(slot, 0, &graph_paths);
            }
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

/// What the `inputs` of a slot lead to, in the actions of the targets that own them.
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

/// The `.bzl` files `imports`, and those that they load, directly or through other `.bzl`
/// files.
async fn loaded_files(
    dice: &mut DiceComputations<'_>,
    cell_resolver: &CellResolver,
    imports: Vec<ImportPath>,
) -> buck2_error::Result<BTreeMap<ProjectRelativePathBuf, CellPath>> {
    let mut seen: BuckMutSet<ImportPath> = BuckMutSet::default();
    let mut queue: VecDeque<ImportPath> = VecDeque::new();
    for import in imports {
        if seen.insert(import.clone()) {
            queue.push_back(import);
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
