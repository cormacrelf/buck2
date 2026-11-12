# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# pyre-strict

# What the server's command and each output may be built from, `stale`, and `DevInfo.scopes`.
# `buck2 dev` builds an output only when the server asks for it. After a change, it sends `stale`
# with each output that the server asked for and that may be built from what changed, and the
# scopes that hold every change since the server last got it. They use the fixture of
# `test_dev`:
#
# - `//app:outputs` has `site`, built from `app/main.txt` and `//lib:lib`, and `extra`, built
#   from `app/extra.txt`. `//app:outputs_slow` is the same, with a delay for `site` only.
# - `//app:scopes` has `site`, `exe` and `patch`, each built from `app/main.txt` and `//lib:lib`,
#   and the scopes `app` (the target itself), `lib` (`//lib:lib`) and `both` (the two). Its
#   server asks for every output when it starts. `//app:scopes_pages` is the same with
#   `app/pages/*.txt`, and `//app:scopes_slow` with a delay.
# - `//app:scopes_site` is `//app:scopes` with the scope `app` only, and a server that asks for
#   `site` only.
# - `//app:patcher` has the same outputs and the scope `lib`, and a server that loads patches:
#   on `stale`, it asks for `patch` if every change to `exe` since it got `exe` is in `lib`, and
#   for `exe` and `patch` if not.
# - `//app:sidecars` has `site`, built from `app/main.txt`, with three sidecars: `extra` and
#   `check`, built from `app/extra.txt` and `app/check.txt`, and `src`, which is
#   `app/main.txt` itself.
#
# Unless a test passes `--manual` to the server, the server asks for every output that `stale`
# lists.

import asyncio
import os
from typing import Any, Dict, List, Tuple

from buck2.tests.core.common.dev.dev_util import (
    APP,
    changed,
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


def sidecars(output: Dict[str, Any]) -> Dict[str, Tuple[str, bool]]:
    """The content of each sidecar of a built output, and whether it changed."""
    return {
        name: (sidecar["content"], sidecar["changed"])
        for name, sidecar in output["sidecars"].items()
    }


def stale(buck: Buck, message: Dict[str, Any]) -> Dict[str, Tuple[List[str], List[str]]]:
    """Each output of a `stale` message, with its `within` and its `files` relative to the
    project root."""
    return {
        name: (entry["within"], files(buck, entry))
        for name, entry in message["outputs"].items()
    }


async def start_manual(
    buck: Buck, target: str
) -> Tuple[DevSession, Dict[str, Dict[str, Any]]]:
    """Start `buck2 dev` with a server that does not answer `stale`, and wait for its first
    build. Returns the `built` message of each output that the server asked for, by output."""
    dev = await start_dev(buck, target, "none", "--", "--manual")
    _, ready, build = await dev.expect("started", "ready", "build")
    results = await dev.results(len(build["outputs"]))
    for result in results.values():
        assert result["type"] == "built", result
        assert result["pid"] == ready["pid"]
    return dev, results


ALL = ["exe", "patch", "site"]


class DevScopesTest(BuckTestCase):
    @dev_test()
    async def test_stale_names_the_outputs_that_may_be_built_from_a_change(
        self, buck: Buck
    ) -> None:
        dev = await start_dev(buck, "//app:outputs")
        try:
            _, ready, _ = await dev.expect("started", "ready", "build")
            assert ready["outputs"] == ["site", "extra"]
            assert ready["scopes"] == []
            assert contents(await dev.results(2)) == {
                "site": "lib v1\napp v1\n",
                "extra": "extra v1\n",
            }

            # Each file makes the outputs that may be built from it stale, and no other. The
            # server asks for them.
            write(buck, "app/main.txt", "app v2\n")
            message, build, built = await dev.expect("stale", "build", "built")
            assert stale(buck, message) == {"site": ([], ["app/main.txt"])}
            assert build["outputs"] == ["site"]
            assert contents(built) == {"site": "lib v1\napp v2\n"}

            write(buck, "app/extra.txt", "extra v2\n")
            message, build, built = await dev.expect("stale", "build", "built")
            assert stale(buck, message) == {"extra": ([], ["app/extra.txt"])}
            assert contents(built) == {"extra": "extra v2\n"}

            # A file of a dependency.
            write(buck, "lib/lib.txt", "lib v2\n")
            message, build, built = await dev.expect("stale", "build", "built")
            assert stale(buck, message) == {"site": ([], ["lib/lib.txt"])}
            assert contents(built) == {"site": "lib v2\napp v2\n"}

            # Both at once.
            write(buck, "app/main.txt", "app v3\n")
            write(buck, "app/extra.txt", "extra v3\n")
            message, build = await dev.expect("stale", "build")
            assert sorted(message["outputs"]) == ["extra", "site"]
            assert build["outputs"] == ["extra", "site"]
            assert contents(await dev.results(2)) == {
                "site": "lib v2\napp v3\n",
                "extra": "extra v3\n",
            }
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_server_file_is_only_checked(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:outputs")
        try:
            first = (await dev.start(outputs=2))["site"]

            # No output is built from the server script, so a save of it makes nothing stale,
            # and the server's command is the same. So is a file that nothing is built from.
            os.utime(buck.cwd / "app/server.py")
            write(buck, "app/unrelated.txt", "unrelated\n")
            write(buck, "app/other.txt", "other v2\n")
            await dev.quiet()

            # New contents start the server again, and the new server asks for what it needs.
            write(buck, "app/server.py", read(buck, "app/server.py") + "# changed\n")
            shutdown, started, _, build = await dev.expect(
                "shutdown", "started", "ready", "build"
            )
            assert shutdown["pid"] == first["pid"]
            assert build["outputs"] == ["extra", "site"]
            results = await dev.results(2)
            assert {result["pid"] for result in results.values()} == {started["pid"]}
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_nothing_is_built_without_a_request(self, buck: Buck) -> None:
        dev, _ = await start_manual(buck, "//app:scopes")
        try:
            write(buck, "app/main.txt", "app v2\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message) == {
                name: (["app", "both"], ["app/main.txt"]) for name in ALL
            }
            await dev.quiet()

            dev.request({"type": "build", "outputs": ["site"]})
            build, built = await dev.expect("build", "built")
            assert build["outputs"] == ["site"]
            assert contents(built) == {"site": "lib v1\napp v2\n"}
            # A build makes nothing stale.
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_ready_names_the_scopes(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:scopes", "none", "--", "--manual")
        try:
            _, ready = await dev.expect("started", "ready")
            assert ready["outputs"] == ["site", "exe", "patch"]
            assert ready["scopes"] == ["app", "both", "lib"]
        finally:
            await dev.stop()

    @dev_test()
    async def test_within_overlapping_scopes(self, buck: Buck) -> None:
        dev, _ = await start_manual(buck, "//app:scopes")
        try:
            # `lib.txt` is an own source file of `//lib:lib`, which is in `lib` and in `both`.
            write(buck, "lib/lib.txt", "lib v2\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message) == {
                name: (["both", "lib"], ["lib/lib.txt"]) for name in ALL
            }
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_within_mixed_changes(self, buck: Buck) -> None:
        dev, _ = await start_manual(buck, "//app:scopes")
        try:
            # One change in `app` and `both`, then one in `lib` and `both`: every change since
            # the server got the outputs is in `both` only.
            write(buck, "app/main.txt", "app v2\n")
            await dev.expect("stale")
            write(buck, "lib/lib.txt", "lib v2\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message) == {
                name: (["both"], ["app/main.txt", "lib/lib.txt"]) for name in ALL
            }

            # And a change in no scope.
            write(buck, "defs.bzl", read(buck, "defs.bzl") + "# changed\n")
            _, message = await dev.expect("graph_changed", "stale")
            assert stale(buck, message) == {
                name: ([], ["app/main.txt", "defs.bzl", "lib/lib.txt"]) for name in ALL
            }

            # The server gets the outputs, and the next change starts again.
            dev.request({"type": "build", "outputs": ALL})
            await dev.expect("build")
            await dev.results(3)
            await dev.quiet()
            write(buck, "lib/lib.txt", "lib v3\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message) == {
                name: (["both", "lib"], ["lib/lib.txt"]) for name in ALL
            }
        finally:
            await dev.stop()

    @dev_test()
    async def test_bzl_change_is_in_no_scope(self, buck: Buck) -> None:
        dev, first = await start_manual(buck, "//app:scopes")
        try:
            # A `.bzl` file may change what any rule does: every output is stale, in no scope,
            # and the server's command is built again.
            write(buck, "defs.bzl", read(buck, "defs.bzl") + "# changed\n")
            _, message = await dev.expect("graph_changed", "stale")
            assert stale(buck, message) == {name: ([], ["defs.bzl"]) for name in ALL}
            assert message["pid"] == first["site"]["pid"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_changed_target_is_in_its_scopes(self, buck: Buck) -> None:
        dev, _ = await start_manual(buck, "//app:scopes")
        try:
            # A file that no target has yet.
            write(buck, "lib/more.txt", "more v1\n")
            await dev.quiet()

            # `lib` changes: it is in `lib` and in `both`.
            edit_target(buck, LIB, "lib", '"lib.txt"', '"lib.txt", "more.txt"')
            _, message = await dev.expect("graph_changed", "stale")
            assert stale(buck, message) == {name: (["both", "lib"], [LIB]) for name in ALL}
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_new_file_of_a_target_in_a_scope(self, buck: Buck) -> None:
        dev, _ = await start_manual(buck, "//app:scopes_pages")
        try:
            # The target's glob takes the new file in, so it is in the target's scopes.
            write(buck, "app/pages/b.txt", "page b\n")
            _, message = await dev.expect("graph_changed", "stale")
            assert stale(buck, message) == {
                name: (["app", "both"], ["app/pages/b.txt"]) for name in ALL
            }

            # And it stays there.
            dev.request({"type": "build", "outputs": ["patch"]})
            _, built = await dev.expect("build", "built")
            assert contents(built) == {"patch": "lib v1\npage a\npage b\n"}
            write(buck, "app/pages/b.txt", "page b2\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message)["patch"] == (["app", "both"], ["app/pages/b.txt"])
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_same_file_saved_twice_sends_stale_twice(self, buck: Buck) -> None:
        dev, _ = await start_manual(buck, "//app:scopes")
        try:
            write(buck, "app/main.txt", "app v2\n")
            (first,) = await dev.expect("stale")
            write(buck, "app/main.txt", "app v2\n")
            (second,) = await dev.expect("stale")
            assert stale(buck, first) == stale(buck, second)
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_output_never_asked_for_is_never_stale(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:scopes_site")
        try:
            await dev.start()
            # `exe` and `patch` are built from `main.txt` too, but the server never asked for
            # them.
            write(buck, "app/main.txt", "app v2\n")
            message, build, built = await dev.expect("stale", "build", "built")
            assert stale(buck, message) == {"site": (["app"], ["app/main.txt"])}
            assert build["outputs"] == ["site"]
            assert built["output"] == "site"
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_within_accumulates_for_the_output_that_was_not_built(
        self, buck: Buck
    ) -> None:
        dev, _ = await start_manual(buck, "//app:scopes")
        try:
            write(buck, "lib/lib.txt", "lib v2\n")
            await dev.expect("stale")

            # The server gets a new `patch`, and not `exe`.
            dev.request({"type": "build", "outputs": ["patch"]})
            await dev.expect("build", "built")
            await dev.quiet()

            write(buck, "lib/lib.txt", "lib v3\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message) == {
                "exe": (["both", "lib"], ["lib/lib.txt"]),
                "patch": (["both", "lib"], ["lib/lib.txt"]),
                "site": (["both", "lib"], ["lib/lib.txt"]),
            }

            # `exe` has every change since the server got it; `patch` only the one since.
            write(buck, "app/main.txt", "app v2\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message)["exe"] == (
                ["both"],
                ["app/main.txt", "lib/lib.txt"],
            )
            assert stale(buck, message)["patch"] == (
                ["both"],
                ["app/main.txt", "lib/lib.txt"],
            )
            dev.request({"type": "build", "outputs": ["patch"]})
            await dev.expect("build", "built")
            write(buck, "lib/lib.txt", "lib v4\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message)["exe"] == (
                ["both"],
                ["app/main.txt", "lib/lib.txt"],
            )
            assert stale(buck, message)["patch"] == (["both", "lib"], ["lib/lib.txt"])
        finally:
            await dev.stop()

    @dev_test()
    async def test_ack_sets_when_the_server_got_an_output(self, buck: Buck) -> None:
        dev, _ = await start_manual(buck, "//app:scopes")
        try:
            write(buck, "app/main.txt", "app v2\n")
            await dev.expect("stale")

            # The server gets `exe`, but has not said that it has it, so it is still stale.
            dev.request({"type": "build", "outputs": ["exe"], "stale_until_ack": True})
            _, built = await dev.expect("build", "built")
            write(buck, "lib/lib.txt", "lib v2\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message)["exe"] == (
                ["both"],
                ["app/main.txt", "lib/lib.txt"],
            )

            # With the `exe` of that build, the server is up to date with `patch` too, which
            # that build did not build. What changed before the build no longer makes them
            # stale; what changed after it still does. A token that no `built` gave, and a name
            # that is not an output, are ignored.
            token = built["token"]
            dev.request({"type": "ack", "token": token, "outputs": ["exe", "patch"]})
            dev.request({"type": "ack", "token": token + 100, "outputs": ["site"]})
            dev.request({"type": "ack", "token": token, "outputs": ["nothing"]})
            # `ack` has no answer. Give the server time to send them.
            await dev.quiet(1)
            write(buck, "lib/lib.txt", "lib v3\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message) == {
                "exe": (["both", "lib"], ["lib/lib.txt"]),
                "patch": (["both", "lib"], ["lib/lib.txt"]),
                "site": (["both"], ["app/main.txt", "lib/lib.txt"]),
            }
        finally:
            output = await dev.stop()
        assert f"with token {token + 100}, which no `built` gave" in output, output
        assert "`nothing`, which is not an output" in output, output

    @dev_test()
    async def test_each_output_is_sent_when_it_is_built(self, buck: Buck) -> None:
        dev, _ = await start_manual(buck, "//app:outputs_slow")
        try:
            # `site` takes 8 seconds to build, and `extra` no time.
            write(buck, "app/delay.txt", "8\n")
            await dev.expect("stale")
            dev.request({"type": "build", "outputs": ["extra", "site"]})
            build, first = await dev.expect("build", "built")
            assert build["outputs"] == ["extra", "site"]
            assert contents(first) == {"extra": "extra v1\n"}

            # `extra` is done, so a change to its file does not stop the build of `site`.
            write(buck, "app/extra.txt", "extra v2\n")
            (last,) = await dev.expect("built")
            assert contents(last) == {"site": "lib v1\napp v1\n"}
            (message,) = await dev.expect("stale")
            assert stale(buck, message) == {"extra": ([], ["app/extra.txt"])}
        finally:
            await dev.stop()

    @dev_test()
    async def test_patcher(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:patcher")
        try:
            await dev.start(outputs=3)

            # A change to `lib`: everything since the binary was built is in `lib`, so the
            # server asks for a patch.
            write(buck, "lib/lib.txt", "lib v2\n")
            message, build, built = await dev.expect("stale", "build", "built")
            assert message["outputs"]["exe"]["within"] == ["lib"]
            assert build["outputs"] == ["patch"]
            assert contents(built) == {"patch": "lib v2\napp v1\n"}

            # Another change to `lib`: `exe` still has only changes in `lib`, and `patch` only
            # the one since its last build.
            write(buck, "lib/lib.txt", "lib v3\n")
            message, build, built = await dev.expect("stale", "build", "built")
            assert build["outputs"] == ["patch"]
            assert files(buck, message["outputs"]["patch"]) == ["lib/lib.txt"]

            # A change elsewhere: the server asks for both, and starts the app again.
            write(buck, "app/main.txt", "app v2\n")
            message, build = await dev.expect("stale", "build")
            assert message["outputs"]["exe"]["within"] == []
            assert build["outputs"] == ["exe", "patch"]
            assert contents(await dev.results(2)) == {
                "exe": "lib v3\napp v2\n",
                "patch": "lib v3\napp v2\n",
            }

            # From the new binary, a change to `lib` is a patch again.
            write(buck, "lib/lib.txt", "lib v4\n")
            message, build, built = await dev.expect("stale", "build", "built")
            assert build["outputs"] == ["patch"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_first_build_that_is_stopped_is_stale(self, buck: Buck) -> None:
        write(buck, "app/delay.txt", "30\n")
        dev = await start_dev(buck, "//app:scopes_slow", "none", "--", "--manual")
        try:
            _, _, build = await dev.expect("started", "ready", "build")
            assert build["outputs"] == ALL
            await asyncio.sleep(2)

            # Each of them is built from both files, so each build stops. The server never got
            # these outputs, so no scope is known for them.
            write(buck, "app/delay.txt", "0\n")
            write(buck, "app/main.txt", "app v2\n")
            results = await dev.results(3, timeout=30)
            assert {name: r["type"] for name, r in results.items()} == {
                name: "cancelled" for name in ALL
            }
            (message,) = await dev.expect("stale")
            assert stale(buck, message) == {
                name: ([], ["app/delay.txt", "app/main.txt"]) for name in ALL
            }
            await dev.quiet()

            dev.request({"type": "build", "outputs": ALL})
            await dev.expect("build")
            assert contents(await dev.results(3)) == {
                name: "lib v1\napp v2\n" for name in ALL
            }
        finally:
            await dev.stop()

    @dev_test()
    async def test_stopped_build_is_stale_and_built_on_request(
        self, buck: Buck
    ) -> None:
        dev, _ = await start_manual(buck, "//app:scopes_slow")
        try:
            # A build of `site` that takes 60 seconds.
            write(buck, "app/delay.txt", "60\n")
            await dev.expect("stale")
            dev.request({"type": "build", "outputs": ["site"]})
            await dev.expect("build")
            await asyncio.sleep(2)

            # `lib` changes while it runs. The build would use the change, so it stops, and
            # `site` is stale with both changes.
            write(buck, "app/delay.txt", "0\n")
            write(buck, "lib/lib.txt", "lib v2\n")
            cancelled, message = await dev.expect("cancelled", "stale", timeout=30)
            assert cancelled["output"] == "site"
            assert stale(buck, message)["site"] == (
                ["both"],
                ["app/delay.txt", "lib/lib.txt"],
            )
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_new_server_has_nothing_stale(self, buck: Buck) -> None:
        dev, _ = await start_manual(buck, "//app:scopes")
        try:
            write(buck, "lib/lib.txt", "lib v2\n")
            await dev.expect("stale")

            # A new server has asked for nothing, so nothing is stale for it: it asks for what
            # it needs, and gets it as the files are.
            write(buck, "app/server.py", read(buck, "app/server.py") + "# changed\n")
            _, _, _, build = await dev.expect("shutdown", "started", "ready", "build")
            assert build["outputs"] == ALL
            assert contents(await dev.results(3))["patch"] == "lib v2\napp v1\n"
            await dev.quiet()

            # The next change is the first since it got them.
            write(buck, "app/main.txt", "app v2\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message) == {
                name: (["app", "both"], ["app/main.txt"]) for name in ALL
            }
        finally:
            await dev.stop()

    @dev_test()
    async def test_failed_build_keeps_the_output_stale(self, buck: Buck) -> None:
        dev, _ = await start_manual(buck, "//app:scopes")
        try:
            write(buck, "lib/lib.txt", "FAIL lib\n")
            await dev.expect("stale")
            dev.request({"type": "build", "outputs": ["patch"]})
            _, failed = await dev.expect("build", "failed")
            assert failed["output"] == "patch"
            await dev.quiet()

            write(buck, "lib/lib.txt", "lib v2\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message)["patch"] == (["both", "lib"], ["lib/lib.txt"])
            write(buck, "app/main.txt", "app v2\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message)["patch"] == (
                ["both"],
                ["app/main.txt", "lib/lib.txt"],
            )

            dev.request({"type": "build", "outputs": ["patch"]})
            _, built = await dev.expect("build", "built")
            assert contents(built) == {"patch": "lib v2\napp v2\n"}
            await dev.quiet()
            write(buck, "app/main.txt", "app v3\n")
            (message,) = await dev.expect("stale")
            assert stale(buck, message)["patch"] == (["app", "both"], ["app/main.txt"])
        finally:
            await dev.stop()

    @dev_test()
    async def test_missed_changes(self, buck: Buck) -> None:
        # A directory that the file watcher watches from the start.
        (buck.cwd / "flood").mkdir()
        dev, _ = await start_manual(buck, "//app:scopes")
        try:
            # Far more files at once than the file watcher's queue holds: it misses changes.
            proc = await asyncio.create_subprocess_exec(
                "sh",
                "-c",
                "for p in $(seq 16); do "
                + '( i=0; while [ $i -lt 6000 ]; do : > "flood/$p.$i"; i=$((i+1)); done ) & '
                + "done; wait",
                cwd=buck.cwd,
            )
            await proc.wait()
            # Any file may have changed, in any scope or none.
            message = await dev.wait_for("stale", timeout=60)
            assert stale(buck, message) == {name: ([], ["."]) for name in ALL}
        finally:
            await dev.stop()

    @dev_test()
    async def test_sidecars_are_built_with_their_output(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:sidecars")
        try:
            _, ready, _, built = await dev.expect("started", "ready", "build", "built")
            # A sidecar is not an output.
            assert ready["outputs"] == ["site"]
            check = read(buck, "app/check.txt")
            assert sidecars(built) == {
                "check": (check, True),
                "extra": ("extra v1\n", True),
                "src": ("app v1\n", True),
            }

            # A file that only a sidecar is built from makes the output stale. The sidecar is
            # new, and the output and the other sidecars are not.
            write(buck, "app/extra.txt", "extra v2\n")
            message, build, built = await dev.expect("stale", "build", "built")
            assert files(buck, message["outputs"]["site"]) == ["app/extra.txt"]
            assert build["outputs"] == ["site"]
            assert changed(built) == {"site": False}
            assert sidecars(built) == {
                "check": (check, False),
                "extra": ("extra v2\n", True),
                "src": ("app v1\n", False),
            }

            # A file of the output, which is a sidecar itself.
            write(buck, "app/main.txt", "app v2\n")
            _, _, built = await dev.expect("stale", "build", "built")
            assert contents(built) == {"site": "app v2\n"}
            assert changed(built) == {"site": True}
            assert sidecars(built) == {
                "check": (check, False),
                "extra": ("extra v2\n", False),
                "src": ("app v2\n", True),
            }

            # The server cannot ask for a sidecar by itself.
            dev.request({"type": "build", "outputs": ["extra"]})
            _, failed = await dev.expect("build", "failed")
            assert failed["output"] == "extra", failed
            assert "no output named `extra`" in failed["error"], failed
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_writes_under_buck_out_do_not_make_outputs_stale(
        self, buck: Buck
    ) -> None:
        dev = await start_dev(buck, "//app:flood")
        try:
            # The action of `flood` writes about a hundred thousand files under `buck-out`.
            # They are no changes to the project: the build is not stopped, and nothing is
            # stale after it.
            assert contents(await dev.start(outputs=2)) == {
                "flood": "app v1\n",
                "site": "app v1\n",
            }
            await dev.quiet(5)

            write(buck, "app/main.txt", "app v2\n")
            message, _ = await dev.expect("stale", "build")
            assert stale(buck, message) == {
                "flood": ([], ["app/main.txt"]),
                "site": ([], ["app/main.txt"]),
            }
            assert contents(await dev.results(2)) == {
                "flood": "app v2\n",
                "site": "app v2\n",
            }
            await dev.quiet(5)
        finally:
            await dev.stop()

    @dev_test()
    async def test_directory_made_in_the_project_root_is_watched(
        self, buck: Buck
    ) -> None:
        dev = await start_dev(buck, "//app:outputs")
        try:
            await dev.start(outputs=2)

            # A package in a directory that the root of the project gets after the daemon
            # started watching, and that `site` is then built from.
            write(buck, "more/more.txt", "more v1\n")
            write(
                buck,
                "more/TARGETS.fixture",
                'load("//:defs.bzl", "concat")\n\n'
                'concat(name = "more", srcs = ["more.txt"], visibility = ["PUBLIC"])\n',
            )
            await dev.quiet()
            edit_target(buck, APP, "outputs", '"//lib:lib"', '"//lib:lib", "//more:more"')
            # The target of both outputs changed, so both are stale.
            _, _, build = await dev.expect("graph_changed", "stale", "build")
            assert build["outputs"] == ["extra", "site"]
            assert contents(await dev.results(2))["site"] == "lib v1\nmore v1\napp v1\n"

            # A change to a file in that directory is seen.
            write(buck, "more/more.txt", "more v2\n")
            message, _, built = await dev.expect("stale", "build", "built")
            assert stale(buck, message) == {"site": ([], ["more/more.txt"])}
            assert contents(built) == {"site": "lib v1\nmore v2\napp v1\n"}
            await dev.quiet()
        finally:
            await dev.stop()
