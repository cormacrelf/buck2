# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# pyre-strict

# Outputs that the server defines while it runs, with the handlers of `DevInfo.handlers`. They
# use the fixture of `test_dev`: `//app:handlers` has `site`, built from `app/main.txt`, the
# scope `lib`, and the handlers `owner`, which builds the default output of the target that owns
# the file `--path`, and `broken`, whose output is no artifact. Both are in `dev.bxl`.

from typing import Any, Dict, List, Optional, Tuple

from buck2.tests.core.common.dev.dev_util import (
    contents,
    DevSession,
    dev_test,
    edit_target,
    files,
    LIB,
    read,
    start_dev,
    write,
)
from buck2.tests.e2e_util.api.buck import Buck
from buck2.tests.e2e_util.buck_workspace import BuckTestCase


async def start_manual(buck: Buck) -> Tuple[DevSession, Dict[str, Any]]:
    """Start `buck2 dev` on `//app:handlers` with a server that does not answer `stale`, and
    wait for its first build."""
    dev = await start_dev(buck, "//app:handlers", "none", "--", "--manual")
    return dev, await dev.start()


def owner(path: str) -> Dict[str, Any]:
    """The call of the handler `owner` for `path`."""
    return {"handler": "owner", "args": ["--path", path]}


def build(dev: DevSession, outputs: Dict[str, Optional[Dict[str, Any]]]) -> None:
    dev.request({"type": "build", "outputs": outputs})


def stale(buck: Buck, message: Dict[str, Any]) -> Dict[str, Tuple[List[str], List[str]]]:
    return {
        name: (entry["within"], files(buck, entry))
        for name, entry in message["outputs"].items()
    }


class DevHandlersTest(BuckTestCase):
    @dev_test()
    async def test_dynamic_outputs(self, buck: Buck) -> None:
        """A dynamic output builds, and is stale after a change to what it is built from, with
        the scopes of the `DevInfo`. A change to another file makes nothing stale."""
        dev = await start_dev(buck, "//app:handlers", "none", "--", "--manual")
        try:
            _, ready, _, _ = await dev.expect("started", "ready", "building", "built")
            assert ready["outputs"] == ["site"]
            assert ready["handlers"] == ["broken", "owner"]

            build(dev, {"lib": owner("lib/lib.txt"), "part": owner("app/part.txt")})
            building, built = await dev.expect("building", "built")
            assert building["outputs"] == ["lib", "part"]
            assert contents(built) == {"lib": "lib v1\n", "part": "part v1\n"}

            # Nothing in the graph of `//app:handlers` is in `lib/`: the change gets through
            # because `lib` is built from it.
            write(buck, "lib/lib.txt", "lib v2\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message) == {"lib": (["lib"], ["lib/lib.txt"])}
            await dev.quiet()

            # Asked for by name, as an output of the `DevInfo` is.
            build(dev, {"lib": None})
            building, built = await dev.expect("building", "built")
            assert files(buck, building) == ["lib/lib.txt"]
            assert contents(built) == {"lib": "lib v2\n"}
            assert built["outputs"]["lib"]["changed"]

            write(buck, "app/part.txt", "part v2\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message) == {"part": ([], ["app/part.txt"])}

            # `app/extra.txt` is in the package of `site` and of `part`, but nothing is built
            # from it.
            write(buck, "app/extra.txt", "extra v2\n")
            await dev.quiet()

            # The same name with another call is another output.
            build(dev, {"part": owner("app/other.txt")})
            building, built = await dev.expect("building", "built")
            assert files(buck, building) == []
            assert contents(built) == {"part": "other v1\n"}
            write(buck, "app/part.txt", "part v3\n")
            await dev.quiet()
            write(buck, "app/other.txt", "other v2\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message) == {"part": ([], ["app/other.txt"])}
        finally:
            await dev.stop()

    @dev_test()
    async def test_forget(self, buck: Buck) -> None:
        """An output that the server forgot is never stale."""
        dev, _ = await start_manual(buck)
        try:
            build(dev, {"lib": owner("lib/lib.txt")})
            await dev.expect("building", "built")
            dev.request({"type": "forget", "outputs": ["lib", "site"]})
            # `buck2 dev` has handled the request.
            assert await dev.sync(30)
            write(buck, "lib/lib.txt", "lib v2\n")
            write(buck, "app/main.txt", "app v2\n")
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_output_that_cannot_be_defined(self, buck: Buck) -> None:
        """An unknown handler, or the name of an output of the `DevInfo`, fails that output, and
        not the others."""
        dev, _ = await start_manual(buck)
        try:
            build(
                dev,
                {
                    "lib": owner("lib/lib.txt"),
                    "nope": {"handler": "nope", "args": []},
                    "site": owner("app/part.txt"),
                },
            )
            _, built = await dev.expect("building", "built")
            assert contents(built) == {"lib": "lib v1\n"}
            assert sorted(built["failed"]) == ["nope", "site"]
            assert "no handler named `nope`" in built["failed"]["nope"]
            assert "is an output of the `DevInfo`" in built["failed"]["site"]

            # Neither is tracked, and `site` is as it was.
            write(buck, "lib/lib.txt", "lib v2\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message) == {"lib": (["lib"], ["lib/lib.txt"])}
            write(buck, "app/main.txt", "app v2\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message) == {
                "lib": (["lib"], ["lib/lib.txt"]),
                "site": ([], ["app/main.txt"]),
            }
        finally:
            await dev.stop()

    @dev_test()
    async def test_handler_that_fails(self, buck: Buck) -> None:
        """A handler whose output is no single artifact fails that output. What it is built
        from is not known, so every change makes it stale."""
        dev, _ = await start_manual(buck)
        try:
            build(dev, {"empty": {"handler": "broken"}, "lib": owner("lib/lib.txt")})
            _, built = await dev.expect("building", "built")
            assert contents(built) == {"lib": "lib v1\n"}
            assert "returned 0 default outputs" in built["failed"]["empty"]

            write(buck, "app/extra.txt", "extra v2\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message) == {"empty": ([], ["app/extra.txt"])}
        finally:
            await dev.stop()

    @dev_test()
    async def test_handler_file_is_a_bzl_file_of_its_outputs(self, buck: Buck) -> None:
        """A change to the file of a handler makes its outputs stale, in no scope. The target
        does not load it, so it is not analysed again."""
        dev, _ = await start_manual(buck)
        try:
            build(dev, {"lib": owner("lib/lib.txt")})
            await dev.expect("building", "built")
            write(buck, "dev.bxl", read(buck, "dev.bxl") + "# changed\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message) == {"lib": ([], ["dev.bxl"])}
            await dev.quiet()

            build(dev, {"lib": None})
            _, built = await dev.expect("building", "built")
            assert contents(built) == {"lib": "lib v1\n"}
            assert not built["outputs"]["lib"]["changed"]
        finally:
            await dev.stop()

    @dev_test()
    async def test_target_change_in_the_graph_of_a_dynamic_output(self, buck: Buck) -> None:
        """A change to a target that a dynamic output is built from makes it stale, in the
        scopes of the target. The graph of `//app:handlers` has no such target, so the server
        does not hear `graph_changed`."""
        dev, _ = await start_manual(buck)
        try:
            build(dev, {"lib": owner("lib/lib.txt")})
            await dev.expect("building", "built")
            # A file that no target has yet.
            write(buck, "lib/more.txt", "more v1\n")
            await dev.quiet()
            edit_target(buck, LIB, "lib", '"lib.txt"', '"lib.txt", "more.txt"')
            (message,) = await dev.expect("stale")
            assert stale(buck, message) == {"lib": (["lib"], [LIB])}
            await dev.quiet()

            build(dev, {"lib": None})
            _, built = await dev.expect("building", "built")
            assert contents(built) == {"lib": "lib v1\nmore v1\n"}
            write(buck, "lib/more.txt", "more v2\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message) == {"lib": (["lib"], ["lib/more.txt"])}
        finally:
            await dev.stop()

    @dev_test()
    async def test_restart_drops_dynamic_outputs(self, buck: Buck) -> None:
        """A server that starts again has defined no output."""
        dev, first = await start_manual(buck)
        try:
            build(dev, {"lib": owner("lib/lib.txt")})
            await dev.expect("building", "built")
            dev.request({"type": "restart"})
            shutdown, _, _, _, built = await dev.expect(
                "shutdown", "started", "ready", "building", "built"
            )
            assert shutdown["pid"] == first["pid"]
            assert contents(built) == {"site": "app v1\n"}
            write(buck, "lib/lib.txt", "lib v2\n")
            await dev.quiet()
        finally:
            await dev.stop()
