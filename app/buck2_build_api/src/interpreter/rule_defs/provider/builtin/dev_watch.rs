/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

//! `DevWatch`: one rule of the `buck2 dev` watch policy in `DevInfo`.

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
use starlark::values::Value;
use starlark::values::ValueOfUnchecked;
use starlark::values::list::AllocList;
use starlark::values::list::ListRef;
use starlark::values::list::ListType;
use starlark::values::list_or_tuple::UnpackListOrTuple;
use starlark::values::none::NoneOr;

use crate as buck2_build_api;

#[derive(Debug, buck2_error::Error)]
#[buck2(tag = Input)]
enum DevWatchError {
    #[error("A `DevWatch` with `notify = True` cannot also set `build` or `restart`")]
    NotifyWithBuild,
    #[error(
        "`DevWatch.targets` must contain target patterns (strings) or target labels, but got `{0}`"
    )]
    BadTarget(String),
}

/// What a `DevWatch` asks for, as plain data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevWatchSpec {
    /// Target patterns whose source files belong to this set. `None` means every target in the
    /// dependency graph that an earlier set did not claim.
    pub targets: Option<Vec<String>>,
    /// Names of the `DevInfo.outputs` to build when a file of this set changes.
    pub build: Vec<String>,
    /// Rebuild and restart the server when a file of this set changes.
    pub restart: bool,
    /// Tell the server about changes to files of this set, and build nothing until it asks.
    pub notify: bool,
}

/// One rule of a `buck2 dev` watch policy: a set of targets, and what to do when one of their
/// source files changes.
#[internal_provider(dev_watch_creator)]
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
pub struct DevWatch<'v> {
    /// Target patterns, or `None` for the rest of the dependency graph.
    targets: ValueOfUnchecked<'v, NoneOr<ListType<String>>>,
    /// Output names to build.
    build: ValueOfUnchecked<'v, ListType<String>>,
    /// Whether to rebuild and restart the server.
    restart: ValueOfUnchecked<'v, bool>,
    /// Whether to only notify the server.
    notify: ValueOfUnchecked<'v, bool>,
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
    Err(DevWatchError::BadTarget(value.to_repr()).into())
}

fn str_list<'v>(value: Value<'v>) -> Vec<String> {
    ListRef::from_value(value)
        .expect("validated at construction")
        .iter()
        .map(|v| {
            v.unpack_str()
                .expect("validated at construction")
                .to_owned()
        })
        .collect()
}

#[starlark_module]
fn dev_watch_creator(globals: &mut GlobalsBuilder) {
    /// A set of targets for `buck2 dev`, and what to do when one of their source files changes.
    ///
    /// * `targets`: target patterns (`"root//lib/..."`) or labels. `None` stands for every
    ///   target in the dependency graph that an earlier `DevWatch` did not claim.
    /// * `build`: names of `DevInfo.outputs` to build after a change.
    /// * `restart`: rebuild the server and start it again after a change.
    /// * `notify`: only tell the server; build nothing until it asks.
    ///
    /// A `DevWatch` that sets none of `build`, `restart` and `notify` makes `buck2 dev` ignore
    /// changes to its files.
    #[starlark(as_type = DevWatch<'static>)]
    fn DevWatch<'v>(
        #[starlark(require = named, default = NoneOr::None)] targets: NoneOr<
            UnpackListOrTuple<Value<'v>>,
        >,
        #[starlark(require = named, default = UnpackListOrTuple::default())]
        build: UnpackListOrTuple<String>,
        #[starlark(require = named, default = false)] restart: bool,
        #[starlark(require = named, default = false)] notify: bool,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<DevWatch<'v>> {
        let heap = eval.heap();
        if notify && (restart || !build.items.is_empty()) {
            return Err(buck2_error::Error::from(DevWatchError::NotifyWithBuild).into());
        }
        let targets = match targets.into_option() {
            None => Value::new_none(),
            Some(targets) => {
                let patterns = targets
                    .items
                    .into_iter()
                    .map(target_to_pattern)
                    .collect::<buck2_error::Result<Vec<_>>>()?;
                heap.alloc(AllocList(patterns))
            }
        };
        Ok(DevWatch {
            targets: ValueOfUnchecked::new(targets),
            build: ValueOfUnchecked::new(heap.alloc(AllocList(build.items))),
            restart: heap.alloc_typed_unchecked(restart).cast(),
            notify: heap.alloc_typed_unchecked(notify).cast(),
        })
    }
}

impl<'v> DevWatch<'v> {
    pub fn spec(&self) -> DevWatchSpec {
        let targets = self.targets.get();
        DevWatchSpec {
            targets: if targets.is_none() {
                None
            } else {
                Some(str_list(targets))
            },
            build: str_list(self.build.get()),
            restart: self.restart.unpack().expect("validated at construction"),
            notify: self.notify.unpack().expect("validated at construction"),
        }
    }
}
