# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# pyre-strict

# What `buck2 dev` does when builds fail while it runs: the outputs, the server's own build, and
# the server's requests. The fixture's `concat` actions fail when a line of what they
# concatenate starts with `FAIL`. Each test states the behaviour it requires. Some fail against
# the current `buck2 dev`: they describe behaviour it should have. They use the fixture of
# `test_dev`.

import os

from buck2.tests.core.common.dev.dev_util import (
    alive,
    APP,
    assert_kept,
    changed,
    contents,
    dev_test,
    edit_target,
    files,
    site,
    start_dev,
    summary,
    write,
)
from buck2.tests.e2e_util.api.buck import Buck
from buck2.tests.e2e_util.buck_workspace import BuckTestCase

# How long to wait for a message that should come, in tests that fail without it.
EXPECTED_WITHIN = 10


class DevFailuresTest(BuckTestCase):
    @dev_test()
    async def test_unrelated_change_while_server_build_broken_sends_nothing(
        self, buck: Buck
    ) -> None:
        """After the server's build failed on a restart, a change to a file that nothing uses
        sends nothing, as it does while the server builds."""
        dev = await start_dev(buck, "//app:restart_built_server")
        try:
            first = await dev.start()
            write(buck, "app/check.txt", "broken\n")
            await dev.expect("failed")
            await dev.quiet()

            write(buck, "app/unrelated.txt", "unrelated\n")
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_failed_build_keeps_the_server(self, buck: Buck) -> None:
        """An edit that makes the output's action fail: `building` and `failed` with the same
        `gen`, and the server keeps running."""
        dev = await start_dev(buck, "//app:all", "simple")
        try:
            first = await dev.start()
            write(buck, "app/main.txt", "FAIL v2\n")
            building, failed = await dev.expect("building", "failed")
            assert files(buck, building) == ["app/main.txt"]
            assert failed["gen"] == building["gen"]
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            output = await dev.stop()
        # The terminal says why.
        assert "an input says FAIL" in output, output

    @dev_test()
    async def test_failed_build_error_says_why(self, buck: Buck) -> None:
        """The `error` of `failed` for an action that fails has the action's error, as the
        terminal does."""
        dev = await start_dev(buck, "//app:all")
        try:
            await dev.start()
            write(buck, "app/main.txt", "FAIL v2\n")
            _, failed = await dev.expect("building", "failed")
            assert "an input says FAIL" in failed["error"], (
                f"`error` of `failed` is {failed['error']!r}"
            )
        finally:
            await dev.stop()

    @dev_test()
    async def test_fixed_after_failed_build(self, buck: Buck) -> None:
        """Fixing the file after a failed build builds again, and the output has changed."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            write(buck, "app/main.txt", "FAIL v2\n")
            await dev.expect("building", "failed")
            write(buck, "app/main.txt", "app v3\n")
            building, built = await dev.expect("building", "built")
            assert files(buck, building) == ["app/main.txt"]
            assert site(built) == "app v3\n"
            assert changed(built) == {"site": True}
            assert built["pid"] == first["pid"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_reverted_after_failed_build(self, buck: Buck) -> None:
        """Undoing the edit that broke the build builds again, and the output is the same as the
        server last got: `changed` is false."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            write(buck, "app/main.txt", "FAIL v2\n")
            await dev.expect("building", "failed")
            write(buck, "app/main.txt", "app v1\n")
            _, built = await dev.expect("building", "built")
            assert site(built) == "app v1\n"
            assert changed(built) == {"site": False}
            assert built["pid"] == first["pid"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_other_file_changes_while_broken(self, buck: Buck) -> None:
        """Another source file changes while the build is broken: it builds again and fails
        again. Fixing the broken file builds both changes."""
        dev = await start_dev(buck, "//app:pages")
        try:
            first = await dev.start()
            write(buck, "app/pages/a.txt", "FAIL a\n")
            await dev.expect("building", "failed")

            write(buck, "app/pages/b.txt", "page b\n")
            _, building, _ = await dev.expect("graph_changed", "building", "failed")
            assert files(buck, building) == ["app/pages/b.txt"], building

            write(buck, "app/pages/a.txt", "page a2\n")
            building, built = await dev.expect("building", "built")
            assert files(buck, building) == ["app/pages/a.txt"]
            assert site(built) == "page a2\npage b\n"
            assert built["pid"] == first["pid"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_unrelated_change_while_broken(self, buck: Buck) -> None:
        """A change to a file outside the graph while the build is broken: nothing is built
        again. A save of the broken file without a change builds again, and fails again:
        `buck2 dev` does not know that the contents are the same."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            write(buck, "app/main.txt", "FAIL v2\n")
            await dev.expect("building", "failed")
            write(buck, "app/other.txt", "other v2\n")
            await dev.quiet()

            os.utime(buck.cwd / "app/main.txt")
            building, failed = await dev.expect("building", "failed")
            assert files(buck, building) == ["app/main.txt"]
            assert failed["gen"] == building["gen"]
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_server_asks_again_while_broken(self, buck: Buck) -> None:
        """The server asks for the output again while it is broken: it builds again and fails
        again."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            write(buck, "app/main.txt", "FAIL v2\n")
            await dev.expect("building", "failed")
            dev.request({"type": "build", "outputs": ["site"]})
            building, failed = await dev.expect("building", "failed")
            assert building["files"] == []
            assert building["outputs"] == ["site"]
            assert failed["gen"] == building["gen"]
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_one_of_several_outputs_fails(self, buck: Buck) -> None:
        """One of two outputs breaks: its set fails; the other still builds on its own."""
        dev = await start_dev(buck, "//app:multi")
        try:
            first = await dev.start()
            assert contents(first) == {"site": "app v1\n", "extra": "extra v1\n"}

            write(buck, "app/extra.txt", "FAIL extra\n")
            building, _ = await dev.expect("building", "failed")
            assert building["outputs"] == ["extra"]
            assert files(buck, building) == ["app/extra.txt"]

            write(buck, "app/main.txt", "app v2\n")
            building, built = await dev.expect("building", "built")
            assert building["outputs"] == ["site"]
            assert contents(built) == {"site": "app v2\n"}
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_request_for_several_outputs_with_one_broken(
        self, buck: Buck
    ) -> None:
        """The server asks for two outputs, one broken: it still gets a `built` with the one that
        built."""
        dev = await start_dev(buck, "//app:multi")
        try:
            first = await dev.start()
            write(buck, "app/extra.txt", "FAIL extra\n")
            await dev.expect("building", "failed")

            dev.request({"type": "build", "outputs": ["site", "extra"]})
            building, built = await dev.expect(
                "building", "built", timeout=EXPECTED_WITHIN
            )
            assert building["outputs"] == ["extra", "site"]
            assert contents(built).get("site") == "app v1\n", summary([built])
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_server_build_fails_on_restart(self, buck: Buck) -> None:
        """The server's own build fails when a change restarts it: `failed`, and the old server
        keeps running, until a fix restarts it."""
        dev = await start_dev(buck, "//app:restart_built_server")
        try:
            first = await dev.start()
            write(buck, "app/check.txt", "broken\n")
            (failed,) = await dev.expect("failed")
            assert "Failed to build the `buck2 dev` server" in failed["error"], failed[
                "error"
            ]
            await dev.quiet()
            assert_kept(dev, first)

            write(buck, "app/main.txt", "app v2\n")
            await dev.wait_until_quiet()
            assert_kept(dev, first)

            write(buck, "app/check.txt", "ok\n")
            shutdown, started, _, _, built = await dev.expect(
                "shutdown", "started", "ready", "building", "built"
            )
            assert shutdown["pid"] == first["pid"]
            assert started["pid"] != first["pid"]
            assert site(built) == "app v2\n"
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_change_while_server_build_broken_fails_once(
        self, buck: Buck
    ) -> None:
        """While the server's build is broken, a change that restarts the server sends one
        `failed`."""
        dev = await start_dev(buck, "//app:restart_built_server")
        try:
            first = await dev.start()
            write(buck, "app/check.txt", "broken\n")
            await dev.expect("failed")
            await dev.quiet()

            write(buck, "app/main.txt", "app v2\n")
            await dev.expect("failed")
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_server_build_fails_with_a_build_set(self, buck: Buck) -> None:
        """The server's own build broken, with a set that builds: each change builds the server
        again, which fails, and the old server keeps running. Changes still build the output."""
        dev = await start_dev(buck, "//app:built_server")
        try:
            first = await dev.start()
            write(buck, "app/check.txt", "broken\n")
            failed, building, built = await dev.expect("failed", "building", "built")
            assert "Failed to build the `buck2 dev` server" in failed["error"]
            assert files(buck, building) == ["app/check.txt"]
            assert changed(built) == {"site": False}

            write(buck, "app/main.txt", "app v2\n")
            _, _, built = await dev.expect("failed", "building", "built")
            assert site(built) == "app v2\n"

            # A change to the target does the same.
            edit_target(
                buck,
                APP,
                "built_server",
                'srcs = ["main.txt"],',
                'srcs = ["main.txt", "other.txt"],',
            )
            _, _, built = await dev.expect("failed", "building", "built")
            assert site(built) == "app v2\nother v1\n"
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_rebuilt_server_cannot_start(self, buck: Buck) -> None:
        """The server's new command line names a program that does not exist: the old server is
        stopped, `buck2 dev` keeps running without one, and a fix starts it again."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            targets = edit_target(
                buck,
                APP,
                "all",
                'mode = "all",',
                'mode = "all",\n    server_exe = "no-such-program-for-buck2-dev",',
            )
            (shutdown,) = await dev.expect("shutdown")
            assert shutdown["pid"] == first["pid"]
            await dev.quiet()
            assert dev.running()
            assert not alive(first["pid"])

            write(buck, APP, targets)
            _, _, _, built = await dev.expect("started", "ready", "building", "built")
            assert site(built) == "app v1\n"
            await dev.quiet()
        finally:
            output = await dev.stop()
        assert "no-such-program-for-buck2-dev" in output, output
        assert "starts the server again after the next change" in output, output

    @dev_test()
    async def test_restarted_server_crashes_at_start(self, buck: Buck) -> None:
        """An edit to the server's script that makes it crash at once: the old server is
        stopped, the new one exits, `buck2 dev` keeps running, and the fix starts it again."""
        dev = await start_dev(buck, "//app:inputs")
        try:
            first = await dev.start()
            server = (buck.cwd / "app/server.py").read_text()
            write(buck, "app/server.py", "this is not python (\n" + server)
            (shutdown,) = await dev.expect("shutdown")
            assert shutdown["pid"] == first["pid"]
            await dev.quiet()
            assert dev.running()

            write(buck, "app/server.py", server)
            _, _, _, built = await dev.expect("started", "ready", "building", "built")
            assert site(built) == "app v1\n"
            await dev.quiet()
        finally:
            output = await dev.stop()
        assert "SyntaxError" in output, output
        assert "starts it again after the next change" in output, output

    @dev_test()
    async def test_restart_request_while_server_build_broken(self, buck: Buck) -> None:
        """The server asks for a restart while its own build is broken: `failed`, the old server
        keeps running, and the fix restarts it."""
        dev = await start_dev(buck, "//app:built_server")
        try:
            first = await dev.start()
            write(buck, "app/check.txt", "broken\n")
            await dev.expect("failed", "building", "built")

            dev.request({"type": "restart"})
            (failed,) = await dev.expect("failed")
            assert "Failed to build the `buck2 dev` server" in failed["error"]
            await dev.quiet()
            assert_kept(dev, first)

            write(buck, "app/check.txt", "ok\n")
            shutdown, started = await dev.expect("shutdown", "started")
            assert shutdown["pid"] == first["pid"]
            # The new server gets `ready`, asks for `site`, and gets it.
            rest = await dev.collect()
            assert rest[0]["type"] == "ready", summary(rest)
            assert [e["type"] for e in rest[-2:]] == ["building", "built"], summary(
                rest
            )
            assert rest[-2]["gen"] == rest[-1]["gen"]
            assert all(e["pid"] == started["pid"] for e in rest)
        finally:
            await dev.stop()

    @dev_test()
    async def test_build_request_while_analysis_broken(self, buck: Buck) -> None:
        """The server asks for a build while the analysis of the target fails: one `building`
        and one `failed`."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            edit_target(buck, APP, "all", 'mode = "all"', 'mode = "bogus"')
            await dev.expect("failed")

            await dev.quiet()
            dev.request({"type": "build", "outputs": ["site"]})
            building, failed = await dev.expect("building", "failed")
            assert building["gen"] == failed["gen"]
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_build_request_for_unknown_output(self, buck: Buck) -> None:
        """The server asks for an output that `DevInfo` does not have: `building` and `failed`,
        and the server keeps running."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            dev.request({"type": "build", "outputs": ["nope"]})
            building, failed = await dev.expect("building", "failed")
            assert building["outputs"] == ["nope"]
            assert "no output named `nope`" in failed["error"], failed["error"]
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_build_request_for_no_outputs(self, buck: Buck) -> None:
        """The server asks for a build of no outputs: `building` and an empty `built`."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            dev.request({"type": "build", "outputs": []})
            building, built = await dev.expect("building", "built")
            assert building["outputs"] == []
            assert built["outputs"] == {}
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_failed_requested_build_keeps_the_change_waiting(
        self, buck: Buck
    ) -> None:
        """With a `notify` set, a build that the server asked for and that failed does not end
        the wait: a change to the app's own files builds nothing until a build that the server
        asks for works."""
        dev = await start_dev(buck, "//app:auto")
        try:
            first = await dev.start()
            write(buck, "lib/lib.txt", "FAIL lib\n")
            (stale,) = await dev.expect("stale")
            assert files(buck, stale) == ["lib/lib.txt"]

            dev.request({"type": "build", "outputs": ["site"]})
            await dev.expect("building", "failed")
            await dev.wait_until_quiet()

            write(buck, "app/main.txt", "app v2\n")
            await dev.quiet()

            write(buck, "lib/lib.txt", "lib v1\n")
            await dev.quiet()

            dev.request({"type": "build", "outputs": ["site"]})
            _, built, stale = await dev.expect(
                "building", "built", "stale", timeout=EXPECTED_WITHIN
            )
            assert site(built) == "lib v1\napp v2\n"
            assert stale["files"] == []
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_waiting_change_that_breaks_the_build(self, buck: Buck) -> None:
        """With a `notify` set, a change that waits and breaks the build: `stale`, nothing
        builds until the server asks, and the server's request fails."""
        dev = await start_dev(buck, "//app:auto_build_on_stale")
        try:
            first = await dev.start()
            write(buck, "lib/lib.txt", "FAIL lib\n")
            # The server asks for a build on `stale`.
            stale, building, failed = await dev.expect("stale", "building", "failed")
            assert files(buck, stale) == ["lib/lib.txt"]
            assert building["files"] == []
            assert failed["gen"] == building["gen"]
            await dev.wait_until_quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()
