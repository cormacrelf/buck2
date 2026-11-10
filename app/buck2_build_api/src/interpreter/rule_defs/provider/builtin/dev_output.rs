/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

//! `DevOutput`: one of `DevInfo.outputs`, with the changes after which `buck2 dev` builds it
//! again by itself, and the other artifacts that are built with it.

use allocative::Allocative;
use buck2_build_api_derive::internal_provider;
use buck2_interpreter::types::configured_providers_label::StarlarkConfiguredProvidersLabel;
use buck2_interpreter::types::configured_providers_label::StarlarkProvidersLabel;
use buck2_interpreter::types::target_label::StarlarkConfiguredTargetLabel;
use buck2_interpreter::types::target_label::StarlarkTargetLabel;
use starlark::any::ProvidesStaticType;
use starlark::environment::GlobalsBuilder;
use starlark::eval::Evaluator;
use starlark::values::Freeze;
use starlark::values::StarlarkPagable;
use starlark::values::Trace;
use starlark::values::UnpackValue;
use starlark::values::Value;
use starlark::values::ValueOf;
use starlark::values::ValueOfUnchecked;
use starlark::values::dict::AllocDict;
use starlark::values::dict::DictType;
use starlark::values::dict::UnpackDictEntries;
use starlark::values::list::AllocList;
use starlark::values::list::ListRef;
use starlark::values::list::ListType;
use starlark::values::list_or_tuple::UnpackListOrTuple;
use starlark::values::none::NoneOr;

use crate as buck2_build_api;
use crate::interpreter::rule_defs::artifact::starlark_artifact_like::ValueAsInputArtifactLike;
use crate::interpreter::rule_defs::artifact::starlark_artifact_like::ValueIsInputArtifactAnnotation;

#[derive(Debug, buck2_error::Error)]
#[buck2(tag = Input)]
enum DevOutputError {
    #[error("Sidecar `{name}` of a `DevOutput` must be an artifact, but got `{got}`")]
    ExpectedArtifact { name: String, got: String },
    #[error("Expected target patterns (strings) or target labels, but got `{0}`")]
    BadTarget(String),
}

fn target_to_pattern(value: Value) -> buck2_error::Result<String> {
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

/// One of `DevInfo.outputs`, with the changes after which `buck2 dev` builds it again by itself.
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
    /// The artifact.
    artifact: ValueOfUnchecked<'v, ValueIsInputArtifactAnnotation>,
    /// Target patterns, or `None` for an output that is built again after any change.
    only: ValueOfUnchecked<'v, NoneOr<ListType<String>>>,
    /// Other artifacts that are built with this one, by name.
    sidecars: ValueOfUnchecked<'v, DictType<String, ValueIsInputArtifactAnnotation>>,
}

#[starlark_module]
fn dev_output_creator(globals: &mut GlobalsBuilder) {
    /// An output of `DevInfo`, with the changes after which `buck2 dev` builds it again by
    /// itself.
    ///
    /// * `artifact`: the artifact to build, with its associated artifacts.
    /// * `only`: target patterns (`"root//lib/..."`) or labels. `buck2 dev` builds the output
    ///   again by itself only after a change to the own source files of these targets. After
    ///   any other change that the output may be built from, it stops building the output and
    ///   tells the server that the output is stale, until the server asks for it. An empty
    ///   list means that `buck2 dev` never builds the output by itself. Without `only`, the
    ///   output is built again after every change that it may be built from.
    /// * `sidecars`: other artifacts, by name, that belong to the output: files that say what
    ///   is in it, or the files of a directory one by one. `buck2 dev` builds them whenever
    ///   it builds the output, from the same files, and `built` has the path and the digest
    ///   of each, and whether it changed. What a sidecar is built from counts as what the
    ///   output is built from. The server cannot ask for a sidecar by itself.
    #[starlark(as_type = DevOutput<'static>)]
    fn DevOutput<'v>(
        #[starlark(require = pos)] artifact: ValueOf<'v, ValueIsInputArtifactAnnotation>,
        #[starlark(require = named, default = NoneOr::None)] only: NoneOr<
            UnpackListOrTuple<Value<'v>>,
        >,
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
        let only = match only {
            NoneOr::None => Value::new_none(),
            NoneOr::Other(targets) => {
                let patterns = targets
                    .items
                    .into_iter()
                    .map(target_to_pattern)
                    .collect::<buck2_error::Result<Vec<_>>>()?;
                heap.alloc(AllocList(patterns))
            }
        };
        Ok(DevOutput {
            artifact: artifact.as_unchecked().cast(),
            only: ValueOfUnchecked::new(only),
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

    /// The target patterns of `only`, if it was given.
    pub fn only(&self) -> Option<Vec<String>> {
        let only = self.only.get();
        if only.is_none() {
            return None;
        }
        Some(
            ListRef::from_value(only)
                .expect("validated at construction")
                .iter()
                .map(|v| {
                    v.unpack_str()
                        .expect("validated at construction")
                        .to_owned()
                })
                .collect(),
        )
    }
}
