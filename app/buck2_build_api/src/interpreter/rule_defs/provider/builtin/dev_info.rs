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
use starlark::any::ProvidesStaticType;
use starlark::environment::GlobalsBuilder;
use starlark::eval::Evaluator;
use starlark::values::Freeze;
use starlark::values::FreezeError;
use starlark::values::StarlarkPagable;
use starlark::values::Trace;
use starlark::values::UnpackValue;
use starlark::values::Value;
use starlark::values::ValueOf;
use starlark::values::ValueOfUnchecked;
use starlark::values::ValueTyped;
use starlark::values::dict::DictRef;
use starlark::values::dict::DictType;
use starlark::values::list::AllocList;
use starlark::values::list::ListRef;
use starlark::values::list::ListType;
use starlark::values::list_or_tuple::UnpackListOrTuple;
use starlark::values::none::NoneOr;

use crate as buck2_build_api;
use crate::interpreter::rule_defs::artifact::starlark_artifact_like::ValueAsInputArtifactLike;
use crate::interpreter::rule_defs::artifact::starlark_artifact_like::ValueIsInputArtifactAnnotation;
use crate::interpreter::rule_defs::cmd_args::CommandLineArgLike;
use crate::interpreter::rule_defs::cmd_args::FrozenStarlarkCmdArgs;
use crate::interpreter::rule_defs::cmd_args::StarlarkCmdArgs;
use crate::interpreter::rule_defs::cmd_args::value_as::ValueAsCommandLineLike;
use crate::interpreter::rule_defs::provider::builtin::dev_watch::DevWatch;
use crate::interpreter::rule_defs::provider::builtin::dev_watch::DevWatchSpec;

#[derive(Debug, buck2_error::Error)]
#[buck2(tag = Input)]
enum DevInfoError {
    #[error("`DevInfo.server` is an empty command line")]
    EmptyServer,
    #[error("Expected a dictionary of artifacts, but key `{key}` contained `{got}`")]
    ExpectedArtifact { key: String, got: String },
    #[error("Expected a dictionary with string keys, but got key `{0}`")]
    ExpectedStringKey(String),
    #[error("Output `{key}`: `{artifact}` should not have any associated artifacts")]
    AssociatedArtifacts { key: String, artifact: String },
    #[error("A `DevWatch` builds output `{0}`, which is not in `DevInfo.outputs`")]
    UnknownOutput(String),
    #[error("`DevInfo.watch` must contain `DevWatch` values, but got `{0}`")]
    ExpectedDevWatch(String),
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
    /// The command `buck2 dev` starts once and keeps running.
    server: ValueOfUnchecked<'v, FrozenStarlarkCmdArgs<'static>>,
    /// The outputs `buck2 dev` builds by name.
    outputs: ValueOfUnchecked<'v, DictType<String, ValueIsInputArtifactAnnotation>>,
    /// The watch policy, as a list of `DevWatch`.
    watch: ValueOfUnchecked<'v, ListType<DevWatch<'static>>>,
}

#[starlark_module]
fn dev_info_creator(globals: &mut GlobalsBuilder) {
    /// Makes a target usable with `buck2 dev`.
    ///
    /// * `server`: a `RunInfo` or command line that `buck2 dev` starts and keeps running. It
    ///   reads JSON lines from stdin and may write requests to stdout as JSON lines.
    /// * `outputs`: artifacts that `buck2 dev` builds by name.
    /// * `watch`: a list of `DevWatch`. For a changed file, the first `DevWatch` whose files
    ///   include it decides what happens.
    #[starlark(as_type = DevInfo<'static>)]
    fn DevInfo<'v>(
        #[starlark(require = named)] server: Value<'v>,
        #[starlark(require = named, default = NoneOr::None)] outputs: NoneOr<
            ValueOf<'v, DictType<&'v str, ValueIsInputArtifactAnnotation>>,
        >,
        #[starlark(require = named, default = UnpackListOrTuple::default())]
        watch: UnpackListOrTuple<Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<DevInfo<'v>> {
        let heap = eval.heap();
        let server = StarlarkCmdArgs::try_from_value(server)?;
        let outputs =
            match outputs.into_option() {
                Some(outputs) => outputs.as_unchecked().cast(),
                None => ValueOfUnchecked::new(heap.alloc(starlark::values::dict::AllocDict(
                    Vec::<(Value, Value)>::new(),
                ))),
            };
        let info = DevInfo {
            server: ValueOfUnchecked::new(heap.alloc(server)),
            outputs,
            watch: ValueOfUnchecked::new(heap.alloc(AllocList(watch.items))),
        };
        validate_dev_info(&info)?;
        Ok(info)
    }
}

impl<'v> DevInfo<'v> {
    pub fn server_command_line(&self) -> &'v dyn CommandLineArgLike<'v> {
        ValueAsCommandLineLike::unpack_value_err(self.server.get())
            .expect("validated at construction")
            .0
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

    pub fn outputs(&self) -> buck2_error::Result<BuckMutInsertionMap<String, Artifact>> {
        Self::outputs_iter(&self.outputs_dict())
            .map(|x| {
                let (k, v) = x?;
                Ok((
                    k.to_owned(),
                    v.0.get_bound_artifact()
                        .with_buck_error_context(|| format!("For output `{k}`"))?,
                ))
            })
            .collect()
    }

    fn watch_values(&self) -> buck2_error::Result<Vec<ValueTyped<'v, DevWatch<'v>>>> {
        ListRef::from_value(self.watch.get())
            .expect("Value is a List")
            .iter()
            .map(|v| {
                ValueTyped::<DevWatch>::new(v)
                    .ok_or_else(|| DevInfoError::ExpectedDevWatch(v.to_repr()).into())
            })
            .collect()
    }

    pub fn watch(&self) -> buck2_error::Result<Vec<DevWatchSpec>> {
        Ok(self
            .watch_values()?
            .into_iter()
            .map(|w| w.as_ref().spec())
            .collect::<buck2_error::Result<_>>()?)
    }
}

fn validate_dev_info<'v>(info: &DevInfo<'v>) -> buck2_error::Result<()> {
    let server = StarlarkCmdArgs::try_from_value(info.server.get())?;
    if server.is_empty() {
        return Err(DevInfoError::EmptyServer.into());
    }
    let mut names = Vec::new();
    for x in DevInfo::outputs_iter(&info.outputs_dict()) {
        let (k, v) = x?;
        if let Some(other_artifacts) = v.0.get_associated_artifacts() {
            if !other_artifacts.is_empty() {
                return Err(DevInfoError::AssociatedArtifacts {
                    key: k.to_owned(),
                    artifact: v.0.to_string(),
                }
                .into());
            }
        }
        names.push(k);
    }
    for watch in info.watch()? {
        for name in &watch.build {
            if !names.contains(&name.as_str()) {
                return Err(DevInfoError::UnknownOutput(name.clone()).into());
            }
        }
    }
    Ok(())
}
