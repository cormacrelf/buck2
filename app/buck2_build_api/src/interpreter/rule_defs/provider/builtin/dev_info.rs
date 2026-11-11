/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

//! `DevInfo`: the provider that `buck2 dev` reads.

use allocative::Allocative;
use buck2_artifact::artifact::artifact_type::Artifact;
use buck2_build_api_derive::internal_provider;
use buck2_core::provider::label::ConfiguredProvidersLabel;
use buck2_error::BuckErrorContext;
use buck2_hash::BuckMutInsertionMap;
use buck2_interpreter::types::configured_providers_label::StarlarkConfiguredProvidersLabel;
use dupe::Dupe;
use dupe::IterDupedExt;
use either::Either;
use starlark::any::ProvidesStaticType;
use starlark::environment::GlobalsBuilder;
use starlark::eval::Evaluator;
use starlark::values::Freeze;
use starlark::values::FreezeError;
use starlark::values::Heap;
use starlark::values::OwnedFrozen;
use starlark::values::StarlarkPagable;
use starlark::values::Trace;
use starlark::values::UnpackValue;
use starlark::values::Value;
use starlark::values::ValueOfUnchecked;
use starlark::values::ValueTyped;
use starlark::values::dict::AllocDict;
use starlark::values::dict::DictRef;
use starlark::values::dict::DictType;
use starlark::values::dict::UnpackDictEntries;
use starlark::values::list::AllocList;
use starlark::values::list::ListRef;
use starlark::values::list::ListType;
use starlark::values::list_or_tuple::UnpackListOrTuple;
use starlark::values::none::NoneType;

use crate as buck2_build_api;
use crate::artifact_groups::ArtifactGroup;
use crate::interpreter::rule_defs::artifact::starlark_artifact_like::ValueAsInputArtifactLike;
use crate::interpreter::rule_defs::artifact::starlark_artifact_like::ValueIsInputArtifactAnnotation;
use crate::interpreter::rule_defs::cmd_args::CommandLineArgLike;
use crate::interpreter::rule_defs::cmd_args::FrozenStarlarkCmdArgs;
use crate::interpreter::rule_defs::cmd_args::SimpleCommandLineArtifactVisitor;
use crate::interpreter::rule_defs::cmd_args::StarlarkCmdArgs;
use crate::interpreter::rule_defs::cmd_args::value_as::ValueAsCommandLineLike;
use crate::interpreter::rule_defs::provider::builtin::dev_output::DevOutput;
use crate::interpreter::rule_defs::provider::builtin::dev_output::target_to_pattern;
use crate::interpreter::rule_defs::provider::builtin::external_runner_test_info::check_all;
use crate::interpreter::rule_defs::provider::builtin::external_runner_test_info::iter_local_resources;
use crate::interpreter::rule_defs::provider::builtin::external_runner_test_info::unwrap_all;

#[derive(Debug, buck2_error::Error)]
#[buck2(tag = Input)]
enum DevInfoError {
    #[error("`DevInfo.server` is an empty command line")]
    EmptyServer,
    #[error("Expected a dictionary of artifacts, but key `{key}` contained `{got}`")]
    ExpectedArtifact { key: String, got: String },
    #[error(
        "A `DevInfo` output must be an artifact, or a command line such as a `RunInfo`, but got `{got}`"
    )]
    ExpectedOutput { got: String },
    #[error("A `DevInfo` output is an empty command line")]
    EmptyCommand,
    #[error("Expected a dictionary with string keys, but got key `{0}`")]
    ExpectedStringKey(String),
}

/// An artifact of a `DevInfo`, an output or a sidecar, with the artifacts associated with it
/// (`artifact.with_associated_artifacts()`, `ctx.actions.write(..., with_inputs = True)`).
/// `buck2 dev` builds and materialises those with it.
#[derive(Clone)]
pub struct DevArtifact {
    pub artifact: Artifact,
    pub associated: Vec<ArtifactGroup>,
}

impl DevArtifact {
    fn new(artifact: &ValueAsInputArtifactLike<'_>) -> buck2_error::Result<DevArtifact> {
        Ok(DevArtifact {
            artifact: artifact.0.get_bound_artifact()?,
            associated: artifact
                .0
                .get_associated_artifacts()
                .into_iter()
                .flat_map(|associated| associated.iter())
                .duped()
                .collect(),
        })
    }

    /// The artifact, and then the associated artifacts.
    pub fn groups(&self) -> Vec<ArtifactGroup> {
        let mut groups = Vec::with_capacity(1 + self.associated.len());
        groups.push(ArtifactGroup::Artifact(self.artifact.dupe()));
        groups.extend(self.associated.iter().duped());
        groups
    }
}

/// An output of a `DevInfo`.
#[derive(Clone)]
pub enum DevOutputSpec {
    /// An artifact.
    Artifact(DevArtifact),
    /// A command line, such as a `RunInfo`, with the artifacts that it names, hidden ones too.
    /// `buck2 dev` builds those, and resolves the command line to `argv`.
    Command(Vec<ArtifactGroup>),
}

impl DevOutputSpec {
    /// What `buck2 dev` builds and materialises for the output.
    pub fn groups(&self) -> Vec<ArtifactGroup> {
        match self {
            DevOutputSpec::Artifact(artifact) => artifact.groups(),
            DevOutputSpec::Command(inputs) => inputs.clone(),
        }
    }
}

/// An output as `DevInfo` keeps it: an artifact as it is, and anything else that `cmd_args`
/// takes as a `cmd_args`.
pub(crate) fn dev_output_value<'v>(
    heap: Heap<'v>,
    output: Value<'v>,
) -> buck2_error::Result<Value<'v>> {
    if ValueAsInputArtifactLike::unpack_value(output)?.is_some() {
        return Ok(output);
    }
    let command =
        StarlarkCmdArgs::try_from_value(output).map_err(|_| DevInfoError::ExpectedOutput {
            got: output.get_type().to_owned(),
        })?;
    if command.is_empty() {
        return Err(DevInfoError::EmptyCommand.into());
    }
    Ok(heap.alloc(command))
}

/// Provider that makes a target usable with `buck2 dev`.
#[internal_provider(dev_info_creator)]
#[derive(
    Clone,
    Debug,
    Freeze,
    Trace,
    ProvidesStaticType,
    Allocative,
    StarlarkPagable
)]
#[repr(C)]
#[freeze(validator = validate_dev_info)]
pub struct DevInfo<'v> {
    /// The command `buck2 dev` starts and keeps running, and starts again when it changes.
    server: ValueOfUnchecked<'v, FrozenStarlarkCmdArgs<'static>>,
    /// The outputs `buck2 dev` builds by name: artifacts, and command lines.
    outputs: ValueOfUnchecked<
        'v,
        DictType<String, Either<ValueIsInputArtifactAnnotation, FrozenStarlarkCmdArgs<'static>>>,
    >,
    /// Named sets of targets, as target patterns. A change says which of them it is in.
    scopes: ValueOfUnchecked<'v, DictType<String, ListType<String>>>,
    /// For each output that was given as a `DevOutput` with `sidecars`: those, by name.
    sidecars:
        ValueOfUnchecked<'v, DictType<String, DictType<String, ValueIsInputArtifactAnnotation>>>,
    /// Whether the server gets the terminal: its stdin, and the keys typed into it.
    interactive: ValueOfUnchecked<'v, bool>,
    /// The local resources that `buck2 dev` sets up before it starts the server: a name for each,
    /// with the label of a target that has a `LocalResourceInfo`, as in
    /// `ExternalRunnerTestInfo.local_resources`. A name without a label is ignored.
    local_resources:
        ValueOfUnchecked<'v, DictType<String, Option<StarlarkConfiguredProvidersLabel>>>,
}

#[starlark_module]
fn dev_info_creator(globals: &mut GlobalsBuilder) {
    /// Makes a target usable with `buck2 dev`.
    ///
    /// * `server`: a `RunInfo` or command line that `buck2 dev` starts and keeps running. It
    ///   may connect to the socket named in the `BUCK2_DEV_SOCKET` environment variable, to get
    ///   messages from `buck2 dev` and send it requests, as JSON lines. `buck2 dev` builds the
    ///   command again after each change, and starts the server again when the command line,
    ///   or the contents of an artifact that it names, changed. So keep what changes often
    ///   out of it: make that one of `outputs`, which the server asks for once it runs.
    /// * `outputs`: artifacts that `buck2 dev` builds by name, when the server asks for them.
    ///   After a change that an output that the server asked for may be built from,
    ///   `buck2 dev` tells the server that it is stale. An artifact with associated artifacts
    ///   is built with them, and they count as part of the output. An output may also be a
    ///   `RunInfo` or another command line: `buck2 dev` builds what it names, hidden artifacts
    ///   too, and gives the server its `argv`. Give an output as a `DevOutput` to build more
    ///   artifacts with it.
    /// * `scopes`: named sets of targets, each a list of target patterns (`"root//lib/..."`)
    ///   or labels. A change is in a scope when it is to an own source file of one of its
    ///   targets, or to one of its targets. For each stale output, `buck2 dev` tells the
    ///   server the scopes that hold every change since the server last got it.
    /// * `interactive`: give the server the terminal, for a server that reads keys or lines
    ///   from it, such as a REPL. Otherwise its stdin is empty, and Ctrl-C reaches only
    ///   `buck2 dev`.
    /// * `local_resources`: processes that the server needs, such as a database, by name: a
    ///   dictionary from a name to the label of a target with a `LocalResourceInfo`, as in
    ///   `ExternalRunnerTestInfo`. The daemon runs the setup command of each before
    ///   `buck2 dev` starts the server, which gets the environment variables that the
    ///   `LocalResourceInfo` maps. A resource keeps running while the server restarts, and is
    ///   set up again when its setup command is new or it exited. The daemon releases them when
    ///   `buck2 dev` exits.
    #[starlark(as_type = DevInfo<'static>)]
    fn DevInfo<'v>(
        #[starlark(require = named)] server: Value<'v>,
        #[starlark(require = named, default = UnpackDictEntries::default())]
        outputs: UnpackDictEntries<Value<'v>, Value<'v>>,
        #[starlark(require = named, default = UnpackDictEntries::default())]
        scopes: UnpackDictEntries<&'v str, UnpackListOrTuple<Value<'v>>>,
        #[starlark(require = named, default = false)] interactive: bool,
        #[starlark(require = named, default = NoneType)] local_resources: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<DevInfo<'v>> {
        let heap = eval.heap();
        let server = StarlarkCmdArgs::try_from_value(server)?;
        // A `DevOutput` is taken apart here: its artifact goes to `outputs`, and its sidecars
        // to a dictionary of their own.
        let mut artifacts: Vec<(Value<'v>, Value<'v>)> = Vec::new();
        let mut sidecars: Vec<(Value<'v>, Value<'v>)> = Vec::new();
        for (name, output) in outputs.entries {
            match ValueTyped::<DevOutput>::new(output) {
                Some(output) => {
                    // `DevOutput` keeps it as `dev_output_value` does.
                    artifacts.push((name, output.as_ref().artifact()));
                    let with = output.as_ref().sidecars();
                    if DictRef::from_value(with).is_some_and(|with| !with.is_empty()) {
                        sidecars.push((name, with));
                    }
                }
                None => artifacts.push((
                    name,
                    dev_output_value(heap, output)
                        .with_buck_error_context(|| format!("For output `{}`", name.to_str()))?,
                )),
            }
        }
        let scopes = scopes
            .entries
            .into_iter()
            .map(|(name, targets)| {
                let patterns = targets
                    .items
                    .into_iter()
                    .map(target_to_pattern)
                    .collect::<buck2_error::Result<Vec<_>>>()
                    .with_buck_error_context(|| format!("For scope `{name}`"))?;
                Ok((name, heap.alloc(AllocList(patterns))))
            })
            .collect::<buck2_error::Result<Vec<_>>>()?;
        let info = DevInfo {
            server: ValueOfUnchecked::new(heap.alloc(server)),
            outputs: ValueOfUnchecked::new(heap.alloc(AllocDict(artifacts))),
            scopes: ValueOfUnchecked::new(heap.alloc(AllocDict(scopes))),
            sidecars: ValueOfUnchecked::new(heap.alloc(AllocDict(sidecars))),
            interactive: heap.alloc_typed_unchecked(interactive).cast(),
            local_resources: ValueOfUnchecked::new(local_resources),
        };
        validate_dev_info(&info)?;
        Ok(info)
    }
}

/// A `DevInfo` kept alive by its owning frozen heap; usable across threads and awaits.
pub type OwnedDevInfo = OwnedFrozen<ValueTyped<'static, DevInfo<'static>>>;

impl<'v> DevInfo<'v> {
    pub fn server_command_line(&self) -> &'v dyn CommandLineArgLike<'v> {
        ValueAsCommandLineLike::unpack_value_err(self.server.get())
            .expect("validated at construction")
            .0
    }

    pub fn interactive(&self) -> bool {
        self.interactive
            .unpack()
            .expect("validated at construction")
    }

    fn outputs_iter<'a>(
        outputs: &'a DictRef<'v>,
    ) -> impl Iterator<Item = buck2_error::Result<(&'v str, ValueAsInputArtifactLike<'v>)>> + 'a
    {
        outputs.iter().map(|(k, v)| {
            let k = k
                .unpack_str()
                .ok_or_else(|| DevInfoError::ExpectedStringKey(k.to_string()))?;
            Ok((
                k,
                ValueAsInputArtifactLike::unpack_value(v)?.ok_or_else(|| {
                    DevInfoError::ExpectedArtifact {
                        key: k.to_owned(),
                        got: v.get_type().to_owned(),
                    }
                })?,
            ))
        })
    }

    fn outputs_dict(&self) -> DictRef<'v> {
        DictRef::from_value(self.outputs.get()).expect("Value is a Dict")
    }

    /// The outputs, by name, each an artifact or a command line.
    fn outputs_values(&self) -> impl Iterator<Item = buck2_error::Result<(&'v str, Value<'v>)>> {
        self.outputs_dict()
            .iter()
            .map(|(k, v)| {
                let k = k
                    .unpack_str()
                    .ok_or_else(|| DevInfoError::ExpectedStringKey(k.to_string()))?;
                Ok((k, v))
            })
            .collect::<Vec<_>>()
            .into_iter()
    }

    pub fn outputs(&self) -> buck2_error::Result<BuckMutInsertionMap<String, DevOutputSpec>> {
        self.outputs_values()
            .map(|x| {
                let (k, v) = x?;
                let output = match ValueAsInputArtifactLike::unpack_value(v)? {
                    Some(artifact) => DevOutputSpec::Artifact(
                        DevArtifact::new(&artifact)
                            .with_buck_error_context(|| format!("For output `{k}`"))?,
                    ),
                    None => {
                        let mut visitor = SimpleCommandLineArtifactVisitor::new();
                        Self::command_line(v).visit_artifacts(&mut visitor)?;
                        DevOutputSpec::Command(visitor.inputs.into_iter().collect())
                    }
                };
                Ok((k.to_owned(), output))
            })
            .collect()
    }

    fn command_line(value: Value<'v>) -> &'v dyn CommandLineArgLike<'v> {
        ValueAsCommandLineLike::unpack_value_err(value)
            .expect("validated at construction")
            .0
    }

    /// The command line of output `name`, if it is one.
    pub fn output_command_line(&self, name: &str) -> Option<&'v dyn CommandLineArgLike<'v>> {
        let value = self.outputs_dict().get_str(name)?;
        match ValueAsInputArtifactLike::unpack_value(value) {
            Ok(None) => Some(Self::command_line(value)),
            _ => None,
        }
    }

    /// The sidecars of each output that has some, by name.
    pub fn sidecars(
        &self,
    ) -> buck2_error::Result<BuckMutInsertionMap<String, Vec<(String, DevArtifact)>>> {
        DictRef::from_value(self.sidecars.get())
            .expect("Value is a Dict")
            .iter()
            .map(|(output, sidecars)| {
                let output = output.unpack_str().expect("validated at construction");
                let sidecars = DictRef::from_value(sidecars).expect("validated at construction");
                let sidecars = Self::outputs_iter(&sidecars)
                    .map(|x| {
                        let (k, v) = x?;
                        Ok((
                            k.to_owned(),
                            DevArtifact::new(&v).with_buck_error_context(|| {
                                format!("For sidecar `{k}` of output `{output}`")
                            })?,
                        ))
                    })
                    .collect::<buck2_error::Result<Vec<_>>>()?;
                Ok((output.to_owned(), sidecars))
            })
            .collect()
    }

    /// `local_resources`: each name, with the label of the target that has its
    /// `LocalResourceInfo`. Names without a label are left out.
    pub fn local_resources(&self) -> Vec<(&'v str, &'v ConfiguredProvidersLabel)> {
        unwrap_all(iter_local_resources(self.local_resources.get()))
            .filter_map(|(name, label)| Some((name, label?)))
            .collect()
    }

    /// The scopes, each with its target patterns, in the order that they were given.
    pub fn scopes(&self) -> BuckMutInsertionMap<String, Vec<String>> {
        DictRef::from_value(self.scopes.get())
            .expect("Value is a Dict")
            .iter()
            .map(|(name, patterns)| {
                (
                    name.unpack_str()
                        .expect("validated at construction")
                        .to_owned(),
                    ListRef::from_value(patterns)
                        .expect("validated at construction")
                        .iter()
                        .map(|pattern| {
                            pattern
                                .unpack_str()
                                .expect("validated at construction")
                                .to_owned()
                        })
                        .collect(),
                )
            })
            .collect()
    }
}

fn validate_dev_info<'v>(info: &DevInfo<'v>) -> buck2_error::Result<()> {
    let server = StarlarkCmdArgs::try_from_value(info.server.get())?;
    if server.is_empty() {
        return Err(DevInfoError::EmptyServer.into());
    }
    for x in info.outputs_values() {
        let (_, v) = x?;
        if ValueAsInputArtifactLike::unpack_value(v)?.is_none()
            && ValueAsCommandLineLike::unpack_value(v)?.is_none()
        {
            return Err(DevInfoError::ExpectedOutput {
                got: v.get_type().to_owned(),
            }
            .into());
        }
    }
    check_all(iter_local_resources(info.local_resources.get()))?;
    Ok(())
}
