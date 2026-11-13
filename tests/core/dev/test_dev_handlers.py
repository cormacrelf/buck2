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
# the file `--path`, `sidecar`, which does that too and has the output of the owner of
# `--sidecar` as its sidecar `side`, and `broken`, whose output is no artifact. They are in
# `dev.bxl`. The tests send `define` and `build` themselves.

from typing import Any, Dict, List, Tuple

from buck2.tests.core.common.dev.dev_util import (
    contents,
    DevSession,
    dev_test,
    edit_target,
    files,
    LIB,
    read,
    start_dev,
    summary,
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


def define(dev: DevSession, outputs: Dict[str, Dict[str, Any]]) -> None:
    dev.request({"type": "define", "outputs": outputs})


def build(dev: DevSession, outputs: List[str]) -> None:
    dev.request({"type": "build", "outputs": outputs})


def sidecars(output: Dict[str, Any]) -> Dict[str, Tuple[str, bool]]:
    """The content of each sidecar of a built output, and whether it changed."""
    return {
        name: (sidecar["content"], sidecar["changed"])
        for name, sidecar in output["sidecars"].items()
    }


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
            _, ready, _, _ = await dev.expect("started", "ready", "build", "built")
            assert ready["outputs"] == ["site"]
            assert ready["handlers"] == ["broken", "owner", "sidecar"]

            define(dev, {"lib": owner("lib/lib.txt"), "part": owner("app/part.txt")})
            build(dev, ["lib", "part"])
            (message,) = await dev.expect("build")
            assert message["outputs"] == ["lib", "part"]
            results = await dev.results(2)
            assert contents(results) == {"lib": "lib v1\n", "part": "part v1\n"}

            # Nothing in the graph of `//app:handlers` is in `lib/`: the change gets through
            # because `lib` is built from it.
            write(buck, "lib/lib.txt", "lib v2\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message) == {"lib": (["lib"], ["lib/lib.txt"])}
            await dev.quiet()

            build(dev, ["lib"])
            _, built = await dev.expect("build", "built")
            assert contents(built) == {"lib": "lib v2\n"}
            assert built["changed"]

            write(buck, "app/part.txt", "part v2\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message) == {"part": ([], ["app/part.txt"])}

            # `app/extra.txt` is in the package of `site` and of `part`, but nothing is built
            # from it.
            write(buck, "app/extra.txt", "extra v2\n")
            await dev.quiet()

            # The same name with another call is another output, which the server has not got.
            define(dev, {"part": owner("app/other.txt")})
            build(dev, ["part"])
            _, built = await dev.expect("build", "built")
            assert contents(built) == {"part": "other v1\n"}
            write(buck, "app/part.txt", "part v3\n")
            await dev.quiet()
            write(buck, "app/other.txt", "other v2\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message) == {"part": ([], ["app/other.txt"])}
        finally:
            await dev.stop()

    @dev_test()
    async def test_dynamic_output_with_sidecars(self, buck: Buck) -> None:
        """A handler that returns a `DevOutput` gives its output sidecars. They are built with
        it, and a change to what one is built from makes the output stale."""
        dev, _ = await start_manual(buck)
        try:
            call = {
                "handler": "sidecar",
                "args": ["--path", "app/part.txt", "--sidecar", "lib/lib.txt"],
            }
            define(dev, {"part": call})
            build(dev, ["part"])
            _, built = await dev.expect("build", "built")
            assert contents(built) == {"part": "part v1\n"}
            assert sidecars(built) == {"side": ("lib v1\n", True)}

            # Only the sidecar is built from `lib/lib.txt`.
            write(buck, "lib/lib.txt", "lib v2\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message) == {"part": (["lib"], ["lib/lib.txt"])}
            build(dev, ["part"])
            _, built = await dev.expect("build", "built")
            assert not built["changed"]
            assert sidecars(built) == {"side": ("lib v2\n", True)}
        finally:
            await dev.stop()

    @dev_test()
    async def test_forget(self, buck: Buck) -> None:
        """An output that the server forgot is never stale."""
        dev, _ = await start_manual(buck)
        try:
            define(dev, {"lib": owner("lib/lib.txt")})
            build(dev, ["lib"])
            await dev.expect("build", "built")
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
        """An unknown handler, or the name of an output of the `DevInfo`, fails that definition
        at once, and not the others."""
        dev, _ = await start_manual(buck)
        try:
            define(
                dev,
                {
                    "lib": owner("lib/lib.txt"),
                    "nope": {"handler": "nope", "args": []},
                    "site": owner("app/part.txt"),
                },
            )
            failed = await dev.results(2)
            assert sorted(failed) == ["nope", "site"]
            assert all(m["type"] == "failed" for m in failed.values()), summary(
                list(failed.values())
            )
            assert "no handler named `nope`" in failed["nope"]["error"]
            assert "is an output of the `DevInfo`" in failed["site"]["error"]
            await dev.quiet()

            build(dev, ["lib"])
            _, built = await dev.expect("build", "built")
            assert contents(built) == {"lib": "lib v1\n"}

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
        """A handler whose output is no single artifact fails that output, and not the others.
        The output stays defined. What it is built from is not known, so every change makes it
        stale."""
        dev, _ = await start_manual(buck)
        try:
            define(
                dev,
                {"empty": {"handler": "broken", "args": []}, "lib": owner("lib/lib.txt")},
            )
            build(dev, ["empty", "lib"])
            await dev.expect("build")
            results = await dev.results(2)
            assert results["lib"]["type"] == "built", summary(list(results.values()))
            assert contents(results["lib"]) == {"lib": "lib v1\n"}
            assert results["empty"]["type"] == "failed", summary(list(results.values()))
            error = results["empty"]["error"]
            assert "returned 0 default outputs" in error, error

            write(buck, "app/extra.txt", "extra v2\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message) == {"empty": ([], ["app/extra.txt"])}

            # Still defined: asked for again by name, it fails again.
            build(dev, ["empty"])
            _, failed = await dev.expect("build", "failed")
            assert "returned 0 default outputs" in failed["error"], failed["error"]
        finally:
            await dev.stop()

    @dev_test()
    async def test_handler_file_is_a_bzl_file_of_its_outputs(self, buck: Buck) -> None:
        """A change to the file of a handler makes its outputs stale, in no scope. The target
        does not load it, so it is not analysed again."""
        dev, _ = await start_manual(buck)
        try:
            define(dev, {"lib": owner("lib/lib.txt")})
            build(dev, ["lib"])
            await dev.expect("build", "built")
            write(buck, "dev.bxl", read(buck, "dev.bxl") + "# changed\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message) == {"lib": ([], ["dev.bxl"])}
            await dev.quiet()

            build(dev, ["lib"])
            _, built = await dev.expect("build", "built")
            assert contents(built) == {"lib": "lib v1\n"}
            assert not built["changed"]
        finally:
            await dev.stop()

    @dev_test()
    async def test_target_change_in_the_graph_of_a_dynamic_output(self, buck: Buck) -> None:
        """A change to a target that a dynamic output is built from makes it stale, in the
        scopes of the target. The graph of `//app:handlers` has no such target, so the server
        does not hear `graph_changed`."""
        dev, _ = await start_manual(buck)
        try:
            define(dev, {"lib": owner("lib/lib.txt")})
            build(dev, ["lib"])
            await dev.expect("build", "built")
            # A file that no target has yet.
            write(buck, "lib/more.txt", "more v1\n")
            await dev.quiet()
            edit_target(buck, LIB, "lib", '"lib.txt"', '"lib.txt", "more.txt"')
            (message,) = await dev.expect("stale")
            assert stale(buck, message) == {"lib": (["lib"], [LIB])}
            await dev.quiet()

            build(dev, ["lib"])
            _, built = await dev.expect("build", "built")
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
            define(dev, {"lib": owner("lib/lib.txt")})
            build(dev, ["lib"])
            await dev.expect("build", "built")
            dev.request({"type": "restart"})
            shutdown, _, _, _, built = await dev.expect(
                "shutdown", "started", "ready", "build", "built"
            )
            assert shutdown["pid"] == first["pid"]
            assert contents(built) == {"site": "app v1\n"}
            write(buck, "lib/lib.txt", "lib v2\n")
            await dev.quiet()
        finally:
            await dev.stop()
