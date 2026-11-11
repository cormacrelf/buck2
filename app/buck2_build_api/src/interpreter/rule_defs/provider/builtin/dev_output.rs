/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

//! `DevOutput`: one of `DevInfo.outputs`, with the other artifacts that are built with it.

use allocative::Allocative;
use buck2_build_api_derive::internal_provider;
use buck2_interpreter::types::configured_providers_label::StarlarkConfiguredProvidersLabel;
use buck2_interpreter::types::configured_providers_label::StarlarkProvidersLabel;
use buck2_interpreter::types::target_label::StarlarkConfiguredTargetLabel;
use buck2_interpreter::types::target_label::StarlarkTargetLabel;
use either::Either;
use starlark::any::ProvidesStaticType;
use starlark::environment::GlobalsBuilder;
use starlark::eval::Evaluator;
use starlark::values::Freeze;
use starlark::values::StarlarkPagable;
use starlark::values::Trace;
use starlark::values::UnpackValue;
use starlark::values::Value;
use starlark::values::ValueOfUnchecked;
use starlark::values::dict::AllocDict;
use starlark::values::dict::DictType;
use starlark::values::dict::UnpackDictEntries;

use crate as buck2_build_api;
use crate::interpreter::rule_defs::artifact::starlark_artifact_like::ValueAsInputArtifactLike;
use crate::interpreter::rule_defs::artifact::starlark_artifact_like::ValueIsInputArtifactAnnotation;
use crate::interpreter::rule_defs::cmd_args::FrozenStarlarkCmdArgs;
use crate::interpreter::rule_defs::provider::builtin::dev_info::dev_output_value;

#[derive(Debug, buck2_error::Error)]
#[buck2(tag = Input)]
enum DevOutputError {
    #[error("Sidecar `{name}` of a `DevOutput` must be an artifact, but got `{got}`")]
    ExpectedArtifact { name: String, got: String },
    #[error("Expected target patterns (strings) or target labels, but got `{0}`")]
    BadTarget(String),
}

/// A target pattern as a string, or a target label as a pattern for that target.
pub(crate) fn target_to_pattern(value: Value) -> buck2_error::Result<String> {
    if let Some(s) = value.unpack_str() {
        return Ok(s.to_owned());
    }
    if let Some(label) = value.downcast_ref::<StarlarkTargetLabel>() {
        return Ok(label.label().to_string());
    }
    if let Some(label) = value.downcast_ref::<StarlarkConfiguredTargetLabel>() {
        return Ok(label.label().unconfigured().to_string());
    }
    if let Some(label) = value.downcast_ref::<StarlarkConfiguredProvidersLabel>() {
        return Ok(label.label().target().unconfigured().to_string());
    }
    if let Some(label) = value.downcast_ref::<StarlarkProvidersLabel>() {
        return Ok(label.label().target().to_string());
    }
    Err(DevOutputError::BadTarget(value.to_repr()).into())
}

/// One of `DevInfo.outputs`, with the other artifacts that are built with it.
#[internal_provider(dev_output_creator)]
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
pub struct DevOutput<'v> {
    /// The artifact, or the command line.
    artifact: ValueOfUnchecked<
        'v,
        Either<ValueIsInputArtifactAnnotation, FrozenStarlarkCmdArgs<'static>>,
    >,
    /// Other artifacts that are built with this one, by name.
    sidecars: ValueOfUnchecked<'v, DictType<String, ValueIsInputArtifactAnnotation>>,
}

#[starlark_module]
fn dev_output_creator(globals: &mut GlobalsBuilder) {
    /// An output of `DevInfo`, with the other artifacts that are built with it.
    ///
    /// * `output`: the artifact to build, with its associated artifacts. Or a `RunInfo` or
    ///   another command line: `buck2 dev` builds what it names, and gives the server its
    ///   `argv`.
    /// * `sidecars`: other artifacts, by name, that belong to the output: files that say what
    ///   is in it, or the files of a directory one by one. `buck2 dev` builds them whenever
    ///   it builds the output, from the same files, and `built` has the path and the digest
    ///   of each, and whether it changed. What a sidecar is built from counts as what the
    ///   output is built from. The server cannot ask for a sidecar by itself.
    #[starlark(as_type = DevOutput<'static>)]
    fn DevOutput<'v>(
        #[starlark(require = pos)] output: Value<'v>,
        #[starlark(require = named, default = UnpackDictEntries::default())]
        sidecars: UnpackDictEntries<&'v str, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<DevOutput<'v>> {
        let heap = eval.heap();
        for (name, sidecar) in &sidecars.entries {
            if ValueAsInputArtifactLike::unpack_value(*sidecar)?.is_none() {
                return Err(buck2_error::Error::from(DevOutputError::ExpectedArtifact {
                    name: (*name).to_owned(),
                    got: sidecar.get_type().to_owned(),
                })
                .into());
            }
        }
        let artifact = dev_output_value(heap, output)?;
        Ok(DevOutput {
            artifact: ValueOfUnchecked::new(artifact),
            sidecars: ValueOfUnchecked::new(heap.alloc(AllocDict(sidecars.entries))),
        })
    }
}

impl<'v> DevOutput<'v> {
    pub fn artifact(&self) -> Value<'v> {
        self.artifact.get()
    }

    /// The dictionary of the sidecars.
    pub fn sidecars(&self) -> Value<'v> {
        self.sidecars.get()
    }
}
