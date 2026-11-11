# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# pyre-strict

# `buck2 dev` with a bxl function in place of a target. It uses the fixture of `test_dev`, whose
# `dev.bxl` has the functions:
#
# - `app`, which runs `app/server.py` with two outputs: `site`, the default output of
#   `--target`, and `shout`, which an action of the function makes from `site` in capitals. Its
#   scope `lib` is `//lib:lib`.
# - `nothing`, which returns `None`, and `no_default_info`, which returns a `DevInfo` without a
#   `DefaultInfo`.

from typing import Any, Dict, List, Tuple

from buck2.tests.core.common.dev.dev_util import (
    changed,
    contents,
    DevSession,
    dev_test,
    files,
    read,
    start_dev,
    write,
)
from buck2.tests.e2e_util.api.buck import Buck
from buck2.tests.e2e_util.asserts import expect_failure
from buck2.tests.e2e_util.buck_workspace import BuckTestCase


BXL = "dev.bxl"


async def start_app(buck: Buck) -> DevSession:
    """`buck2 dev` on `dev.bxl:app` for `//app:outputs`, with a server that asks for every
    output."""
    return await start_dev(
        buck,
        "dev.bxl:app",
        "none",
        "--",
        "--target",
        "//app:outputs",
        "--",
        "--all-outputs",
    )


def stale(buck: Buck, message: Dict[str, Any]) -> Dict[str, Tuple[List[str], List[str]]]:
    return {
        name: (entry["within"], files(buck, entry))
        for name, entry in message["outputs"].items()
    }


class DevBxlTest(BuckTestCase):
    @dev_test()
    async def test_bxl_function_in_place_of_a_target(self, buck: Buck) -> None:
        """The providers that the function returns work as a target's: its outputs, also one
        that its own action makes, its scopes, and the arguments after a second `--`, which go to
        the server."""
        dev = await start_app(buck)
        try:
            _, ready, building, built = await dev.expect(
                "started", "ready", "building", "built"
            )
            assert ready["outputs"] == ["shout", "site"]
            assert ready["scopes"] == ["lib"]
            assert building["outputs"] == ["shout", "site"]
            assert contents(built) == {
                "shout": "LIB V1\nAPP V1\n",
                "site": "lib v1\napp v1\n",
            }

            # What the function's own action is built from is what `site` is built from.
            write(buck, "app/main.txt", "app v2\n")
            message, _, built = await dev.expect("stale", "building", "built")
            assert stale(buck, message) == {
                "shout": ([], ["app/main.txt"]),
                "site": ([], ["app/main.txt"]),
            }
            assert contents(built) == {
                "shout": "LIB V1\nAPP V2\n",
                "site": "lib v1\napp v2\n",
            }

            write(buck, "lib/lib.txt", "lib v2\n")
            message, _, built = await dev.expect("stale", "building", "built")
            assert stale(buck, message) == {
                "shout": (["lib"], ["lib/lib.txt"]),
                "site": (["lib"], ["lib/lib.txt"]),
            }
            assert contents(built) == {
                "shout": "LIB V2\nAPP V2\n",
                "site": "lib v2\napp v2\n",
            }

            # `site` is built by an action of `//app:outputs`, from the files that the action
            # uses. `shout` is built by the function's own action, from what `//app:outputs`
            # builds, so it may be built from any file of that target, as an output of a rule
            # may be built from any file of a dependency.
            write(buck, "app/extra.txt", "extra v2\n")
            message, _, built = await dev.expect("stale", "building", "built")
            assert stale(buck, message) == {"shout": ([], ["app/extra.txt"])}
            assert changed(built) == {"shout": False}
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_bxl_file_is_a_bzl_file_of_the_graph(self, buck: Buck) -> None:
        """A change to the function's file analyses it again, and makes every output stale."""
        dev = await start_app(buck)
        try:
            first = await dev.start()
            write(buck, BXL, read(buck, BXL) + "# changed\n")
            _, _, building, built = await dev.expect(
                "graph_changed", "stale", "building", "built"
            )
            assert files(buck, building) == [BXL]
            assert changed(built) == {"shout": False, "site": False}
            assert built["pid"] == first["pid"]

            write(buck, BXL, read(buck, BXL).replace("tr a-z A-Z <", 'sed "s/^/> /" <'))
            _, _, _, built = await dev.expect("graph_changed", "stale", "building", "built")
            assert contents(built) == {
                "shout": "> lib v1\n> app v1\n",
                "site": "lib v1\napp v1\n",
            }
            assert changed(built) == {"shout": True, "site": False}
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_bxl_function_without_providers(self, buck: Buck) -> None:
        await expect_failure(
            buck.run_buck_command("dev", "dev.bxl:nothing"),
            stderr_regex="returned no providers",
        )

    @dev_test()
    async def test_bxl_function_without_default_info(self, buck: Buck) -> None:
        """The function returns providers as a rule does, with a `DefaultInfo`."""
        await expect_failure(
            buck.run_buck_command("dev", "dev.bxl:no_default_info"),
            stderr_regex="did not receive a `DefaultInfo` provider",
        )
