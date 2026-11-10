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
use buck2_error::BuckErrorContext;
use buck2_hash::BuckMutInsertionMap;
use dupe::Dupe;
use dupe::IterDupedExt;
use starlark::any::ProvidesStaticType;
use starlark::environment::GlobalsBuilder;
use starlark::eval::Evaluator;
use starlark::values::Freeze;
use starlark::values::FreezeError;
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

use crate as buck2_build_api;
use crate::artifact_groups::ArtifactGroup;
use crate::interpreter::rule_defs::artifact::starlark_artifact_like::ValueAsInputArtifactLike;
use crate::interpreter::rule_defs::artifact::starlark_artifact_like::ValueIsInputArtifactAnnotation;
use crate::interpreter::rule_defs::cmd_args::CommandLineArgLike;
use crate::interpreter::rule_defs::cmd_args::FrozenStarlarkCmdArgs;
use crate::interpreter::rule_defs::cmd_args::StarlarkCmdArgs;
use crate::interpreter::rule_defs::cmd_args::value_as::ValueAsCommandLineLike;
use crate::interpreter::rule_defs::provider::builtin::dev_output::DevOutput;

#[derive(Debug, buck2_error::Error)]
#[buck2(tag = Input)]
enum DevInfoError {
    #[error("`DevInfo.server` is an empty command line")]
    EmptyServer,
    #[error("Expected a dictionary of artifacts, but key `{key}` contained `{got}`")]
    ExpectedArtifact { key: String, got: String },
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
    /// The outputs `buck2 dev` builds by name.
    outputs: ValueOfUnchecked<'v, DictType<String, ValueIsInputArtifactAnnotation>>,
    /// For each output that was given as a `DevOutput` with `only`: the target patterns.
    only: ValueOfUnchecked<'v, DictType<String, ListType<String>>>,
    /// For each output that was given as a `DevOutput` with `sidecars`: those, by name.
    sidecars:
        ValueOfUnchecked<'v, DictType<String, DictType<String, ValueIsInputArtifactAnnotation>>>,
    /// Whether the server gets the terminal: its stdin, and the keys typed into it.
    interactive: ValueOfUnchecked<'v, bool>,
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
    /// * `outputs`: artifacts that `buck2 dev` builds by name. It builds an output again after
    ///   every change that the output may be built from. Give an output as
    ///   `DevOutput(artifact, only = [...])` to hold it back after some of them. An artifact
    ///   with associated artifacts is built with them, and they count as part of the output.
    /// * `interactive`: give the server the terminal, for a server that reads keys or lines
    ///   from it, such as a REPL. Otherwise its stdin is empty, and Ctrl-C reaches only
    ///   `buck2 dev`.
    #[starlark(as_type = DevInfo<'static>)]
    fn DevInfo<'v>(
        #[starlark(require = named)] server: Value<'v>,
        #[starlark(require = named, default = UnpackDictEntries::default())]
        outputs: UnpackDictEntries<Value<'v>, Value<'v>>,
        #[starlark(require = named, default = false)] interactive: bool,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<DevInfo<'v>> {
        let heap = eval.heap();
        let server = StarlarkCmdArgs::try_from_value(server)?;
        // A `DevOutput` is taken apart here: its artifact goes to `outputs`, and its `only`
        // to a dictionary of its own.
        let mut artifacts: Vec<(Value<'v>, Value<'v>)> = Vec::new();
        let mut only: Vec<(Value<'v>, Value<'v>)> = Vec::new();
        let mut sidecars: Vec<(Value<'v>, Value<'v>)> = Vec::new();
        for (name, output) in outputs.entries {
            match ValueTyped::<DevOutput>::new(output) {
                Some(output) => {
                    artifacts.push((name, output.as_ref().artifact()));
                    if let Some(patterns) = output.as_ref().only() {
                        only.push((name, heap.alloc(AllocList(patterns))));
                    }
                    let with = output.as_ref().sidecars();
                    if DictRef::from_value(with).is_some_and(|with| !with.is_empty()) {
                        sidecars.push((name, with));
                    }
                }
                None => artifacts.push((name, output)),
            }
        }
        let info = DevInfo {
            server: ValueOfUnchecked::new(heap.alloc(server)),
            outputs: ValueOfUnchecked::new(heap.alloc(AllocDict(artifacts))),
            only: ValueOfUnchecked::new(heap.alloc(AllocDict(only))),
            sidecars: ValueOfUnchecked::new(heap.alloc(AllocDict(sidecars))),
            interactive: heap.alloc_typed_unchecked(interactive).cast(),
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

    pub fn outputs(&self) -> buck2_error::Result<BuckMutInsertionMap<String, DevArtifact>> {
        Self::outputs_iter(&self.outputs_dict())
            .map(|x| {
                let (k, v) = x?;
                Ok((
                    k.to_owned(),
                    DevArtifact::new(&v).with_buck_error_context(|| format!("For output `{k}`"))?,
                ))
            })
            .collect()
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

    /// The target patterns of `only`, for each output that has it.
    pub fn only(&self) -> BuckMutInsertionMap<String, Vec<String>> {
        DictRef::from_value(self.only.get())
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
    for x in DevInfo::outputs_iter(&info.outputs_dict()) {
        x?;
    }
    Ok(())
}
