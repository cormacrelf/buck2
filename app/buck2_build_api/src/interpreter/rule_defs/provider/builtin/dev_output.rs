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
//! again by itself.
//!
//! A prototype of the `only` proposal in `dev_review.md`.

use allocative::Allocative;
use buck2_build_api_derive::internal_provider;
use starlark::any::ProvidesStaticType;
use starlark::environment::GlobalsBuilder;
use starlark::eval::Evaluator;
use starlark::values::Freeze;
use starlark::values::StarlarkPagable;
use starlark::values::Trace;
use starlark::values::Value;
use starlark::values::ValueOf;
use starlark::values::ValueOfUnchecked;
use starlark::values::list::AllocList;
use starlark::values::list::ListRef;
use starlark::values::list::ListType;
use starlark::values::list_or_tuple::UnpackListOrTuple;
use starlark::values::none::NoneOr;

use crate as buck2_build_api;
use crate::interpreter::rule_defs::artifact::starlark_artifact_like::ValueIsInputArtifactAnnotation;
use crate::interpreter::rule_defs::provider::builtin::dev_watch::target_to_pattern;

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
}

#[starlark_module]
fn dev_output_creator(globals: &mut GlobalsBuilder) {
    /// An output of `DevInfo`, with the changes after which `buck2 dev` builds it again by
    /// itself.
    ///
    /// * `artifact`: the artifact to build.
    /// * `only`: target patterns (`"root//lib/..."`) or labels. `buck2 dev` builds the output
    ///   again by itself only after a change to the own source files of these targets. After
    ///   any other change that the output may be built from, it stops building the output and
    ///   tells the server that the output is stale, until the server asks for it. An empty
    ///   list means that `buck2 dev` never builds the output by itself. Without `only`, the
    ///   output is built again after every change that it may be built from.
    #[starlark(as_type = DevOutput<'static>)]
    fn DevOutput<'v>(
        #[starlark(require = pos)] artifact: ValueOf<'v, ValueIsInputArtifactAnnotation>,
        #[starlark(require = named, default = NoneOr::None)] only: NoneOr<
            UnpackListOrTuple<Value<'v>>,
        >,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<DevOutput<'v>> {
        let heap = eval.heap();
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
        })
    }
}

impl<'v> DevOutput<'v> {
    pub fn artifact(&self) -> Value<'v> {
        self.artifact.get()
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
