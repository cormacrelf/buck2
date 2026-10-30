# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# OSS stub for `@fbsource//tools/build_defs/testinfra:network_access_utils.bzl`.
# The OSS test runner doesn't sandbox network access, so the per-target value
# is only ever forwarded to `python_unittest`, which drops it.

def _none():
    return None

def _all():
    return None

def _set_package_default(*_args, **_kwargs):
    pass

network_access_utils = struct(
    none = _none,
    all = _all,
    set_package_default = _set_package_default,
)
