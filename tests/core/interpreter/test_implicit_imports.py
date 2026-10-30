# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# pyre-strict


from buck2.tests.e2e_util.api.buck import Buck
from buck2.tests.e2e_util.asserts import expect_failure
from buck2.tests.e2e_util.buck_workspace import buck_test, BuckTestCase, parametrize


class ImplicitImportsTest(BuckTestCase):
    @buck_test()
    async def test_bzl_include_injects_symbols(self, buck: Buck) -> None:
        await buck.targets("//...")

    @buck_test()
    @parametrize("data_file", ["data.toml", "data.json"])
    async def test_data_file_include_is_rejected(
        self, buck: Buck, data_file: str
    ) -> None:
        # Without the check, `.toml` failed with a Starlark parse error inside
        # the data file and `.json` parsed as a Starlark expression, injecting
        # nothing.
        await expect_failure(
            buck.targets("//...", "-c", f"buildfile.includes=//{data_file}"),
            stderr_regex=rf"Implicit import `root//{data_file}` is not a `\.bzl` file",
        )

    @buck_test()
    async def test_format_hint_on_include_is_rejected(self, buck: Buck) -> None:
        # `?as=` belongs to `load()`. An implicit import is always evaluated as
        # Starlark, so a hint there must not be honoured or ignored silently: the
        # path, hint and all, is rejected as an import path.
        await expect_failure(
            buck.targets("//...", "-c", "buildfile.includes=//data.lock?as=toml"),
            stderr_regex=r"Invalid import path `root//data\.lock\?as=toml`",
        )
