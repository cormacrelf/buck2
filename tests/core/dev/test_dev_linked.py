# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# pyre-strict

# Outputs that run only together with other files. They use the package `linked` of the fixture
# of `test_dev`, whose rules are in `linked.bzl`, and whose server (`linked/server.py`) runs each
# output that it gets in `built`, and logs what the output printed:
#
# - `//linked:associated` has `app`, a script with one associated artifact, a copy of
#   `linked/lib.txt`, which the script prints.
# - `//linked:sidecar` is the same, and `app` has a sidecar `check`, another script, with one
#   associated artifact, a copy of `linked/extra.txt`. The server runs the sidecar too.

import os
from typing import Any, Dict

from buck2.tests.core.common.dev.dev_util import dev_test, files, start_dev, write
from buck2.tests.e2e_util.api.buck import Buck
from buck2.tests.e2e_util.buck_workspace import BuckTestCase


def assert_ran(output: Dict[str, Any], stdout: str) -> None:
    """Assert that the output succeeded and printed `stdout` when the server ran it."""
    ran = output["ran"]
    assert ran["code"] == 0 and ran["stdout"] == stdout, ran


class DevLinkedTest(BuckTestCase):
    @dev_test()
    async def test_output_with_associated_artifact(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//linked:associated")
        try:
            built = await dev.start()
            app = built["outputs"]["app"]
            # The associated artifact is there when the server runs the output.
            assert_ran(app, "lib: lib v1\nargs: \n")
            assert app["changed"]
            assert os.path.isabs(app["path"])

            # Only the associated artifact is built from `lib.txt`. A new one is a new output,
            # with the same file.
            write(buck, "linked/lib.txt", "lib v2\n")
            building, built = await dev.expect("building", "built")
            assert building["outputs"] == ["app"]
            assert files(buck, building) == ["linked/lib.txt"]
            new = built["outputs"]["app"]
            assert_ran(new, "lib: lib v2\nargs: \n")
            assert new["path"] == app["path"]
            assert new["content"] == app["content"]
            assert new["changed"]
            assert new["digest"] != app["digest"]

            # The same contents again are no change.
            write(buck, "linked/lib.txt", "lib v2\n")
            building, built = await dev.expect("building", "built")
            assert not built["outputs"]["app"]["changed"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_sidecar_with_associated_artifact(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//linked:sidecar")
        try:
            built = await dev.start()
            app = built["outputs"]["app"]
            check = app["sidecars"]["check"]
            # The sidecar's associated artifact is there too.
            assert_ran(app, "lib: lib v1\nargs: \n")
            assert_ran(check, "extra: extra v1\nargs: \n")
            assert check["changed"]

            # What the sidecar's associated artifact is built from builds the output. The
            # sidecar is new, with the same file, and the output is the same.
            write(buck, "linked/extra.txt", "extra v2\n")
            building, built = await dev.expect("building", "built")
            assert building["outputs"] == ["app"]
            assert files(buck, building) == ["linked/extra.txt"]
            new = built["outputs"]["app"]
            new_check = new["sidecars"]["check"]
            assert_ran(new_check, "extra: extra v2\nargs: \n")
            assert new_check["path"] == check["path"]
            assert new_check["content"] == check["content"]
            assert new_check["changed"]
            assert not new["changed"]
            await dev.quiet()
        finally:
            await dev.stop()
