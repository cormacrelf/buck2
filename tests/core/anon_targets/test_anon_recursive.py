# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# pyre-strict

from buck2.tests.e2e_util.api.buck import Buck
from buck2.tests.e2e_util.buck_workspace import buck_test


@buck_test()
async def test_anon_targets_collect_recursive_js_sources(buck: Buck) -> None:
    target = "root//:all_js_sources"
    report = await buck.build(target)
    output = report.get_build_report().output_for_target(target)

    assert output.read_text().splitlines() == [
        "root.js",
        "left.js",
        "leaf.js",
        "right.js",
    ]
