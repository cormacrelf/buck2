# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# pyre-strict

# What `buck2 dev` does when builds fail while it runs: the outputs, the analysis of the target, the
# server's own build, and the server's requests. The fixture's `concat` actions fail when a line of
# what they concatenate starts with `FAIL`. Each test states the behaviour it requires. Some fail
# against the current `buck2 dev`: they describe behaviour it should have. They use the fixture of
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
    read,
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
        """After the server's build failed, a change to a file that nothing uses sends nothing,
        as it does while the server builds."""
        dev = await start_dev(buck, "//app:built_server")
        try:
            first = await dev.start()
            write(buck, "app/check.txt", "broken\n")
            await dev.expect("error")
            await dev.quiet()

            write(buck, "app/unrelated.txt", "unrelated\n")
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_failed_build_keeps_the_server(self, buck: Buck) -> None:
        """An edit that makes the output's action fail: `stale`, the server asks for the output
        again and gets `failed` for it, and the server keeps running."""
        dev = await start_dev(buck, "//app:all", "simple")
        try:
            first = await dev.start()
            write(buck, "app/main.txt", "FAIL v2\n")
            stale, build, failed = await dev.expect("stale", "build", "failed")
            assert files(buck, stale["outputs"]["site"]) == ["app/main.txt"]
            assert build["outputs"] == ["site"]
            assert failed["output"] == "site"
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
            _, _, failed = await dev.expect("stale", "build", "failed")
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
            await dev.expect("stale", "build", "failed")
            write(buck, "app/main.txt", "app v3\n")
            stale, _, built = await dev.expect("stale", "build", "built")
            assert files(buck, stale["outputs"]["site"]) == ["app/main.txt"]
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
            await dev.expect("stale", "build", "failed")
            write(buck, "app/main.txt", "app v1\n")
            _, _, built = await dev.expect("stale", "build", "built")
            assert site(built) == "app v1\n"
            assert changed(built) == {"site": False}
            assert built["pid"] == first["pid"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_other_file_changes_while_broken(self, buck: Buck) -> None:
        """Another source file changes while the build is broken: the site is stale with both
        changes, builds again and fails again. Fixing the broken file builds both changes."""
        dev = await start_dev(buck, "//app:pages")
        try:
            first = await dev.start()
            write(buck, "app/pages/a.txt", "FAIL a\n")
            await dev.expect("stale", "build", "failed")

            write(buck, "app/pages/b.txt", "page b\n")
            _, stale, _, _ = await dev.expect(
                "graph_changed", "stale", "build", "failed"
            )
            assert files(buck, stale["outputs"]["site"]) == [
                "app/pages/a.txt",
                "app/pages/b.txt",
            ]

            write(buck, "app/pages/a.txt", "page a2\n")
            stale, _, built = await dev.expect("stale", "build", "built")
            assert files(buck, stale["outputs"]["site"]) == [
                "app/pages/a.txt",
                "app/pages/b.txt",
            ]
            assert site(built) == "page a2\npage b\n"
            assert built["pid"] == first["pid"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_unrelated_change_while_broken(self, buck: Buck) -> None:
        """A change to a file outside the graph while the build is broken: nothing is built
        again. A save of the broken file without a change makes the site stale, and it builds
        again and fails again: `buck2 dev` does not know that the contents are the same."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            write(buck, "app/main.txt", "FAIL v2\n")
            await dev.expect("stale", "build", "failed")
            write(buck, "app/other.txt", "other v2\n")
            await dev.quiet()

            os.utime(buck.cwd / "app/main.txt")
            stale, _, failed = await dev.expect("stale", "build", "failed")
            assert files(buck, stale["outputs"]["site"]) == ["app/main.txt"]
            assert failed["output"] == "site"
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
            await dev.expect("stale", "build", "failed")
            dev.request({"type": "build", "outputs": ["site"]})
            build, failed = await dev.expect("build", "failed")
            assert build["outputs"] == ["site"]
            assert failed["output"] == "site"
            assert "an input says FAIL" in failed["error"], failed["error"]
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_one_of_several_outputs_fails(self, buck: Buck) -> None:
        """One of two outputs breaks: its build fails, and it stays stale; the other still
        builds."""
        dev = await start_dev(buck, "//app:multi")
        try:
            first = await dev.start(outputs=2)
            assert contents(first) == {"site": "app v1\n", "extra": "extra v1\n"}

            write(buck, "app/extra.txt", "FAIL extra\n")
            stale, build, failed = await dev.expect("stale", "build", "failed")
            assert build["outputs"] == ["extra"]
            assert failed["output"] == "extra"
            assert files(buck, stale["outputs"]["extra"]) == ["app/extra.txt"]

            write(buck, "app/main.txt", "app v2\n")
            stale, build = await dev.expect("stale", "build")
            assert sorted(stale["outputs"]) == ["extra", "site"]
            assert build["outputs"] == ["extra", "site"]
            results = await dev.results(2)
            assert results["site"]["type"] == "built", summary(list(results.values()))
            assert site(results["site"]) == "app v2\n"
            assert results["extra"]["type"] == "failed", summary(
                list(results.values())
            )
            await dev.quiet()
            assert_kept(dev, first["site"])
        finally:
            await dev.stop()

    @dev_test()
    async def test_request_for_several_outputs_with_one_broken(
        self, buck: Buck
    ) -> None:
        """The server asks for two outputs, one broken: it gets `built` for the one that builds,
        and `failed` for the other."""
        dev = await start_dev(buck, "//app:multi")
        try:
            first = await dev.start(outputs=2)
            write(buck, "app/extra.txt", "FAIL extra\n")
            await dev.expect("stale", "build", "failed")

            dev.request({"type": "build", "outputs": ["site", "extra"]})
            (build,) = await dev.expect("build", timeout=EXPECTED_WITHIN)
            assert build["outputs"] == ["extra", "site"]
            results = await dev.results(2, timeout=EXPECTED_WITHIN)
            assert results["site"]["type"] == "built", summary(list(results.values()))
            assert site(results["site"]) == "app v1\n"
            assert results["extra"]["type"] == "failed", summary(
                list(results.values())
            )
            await dev.quiet()
            assert_kept(dev, first["site"])
        finally:
            await dev.stop()

    @dev_test()
    async def test_server_build_fails_on_restart(self, buck: Buck) -> None:
        """The server's own build fails when a change to its script would restart it: `error`,
        and the old server keeps running, until a fix restarts it."""
        dev = await start_dev(buck, "//app:built_server")
        try:
            first = await dev.start()
            write(buck, "app/check.txt", "broken\n")
            (error,) = await dev.expect("error")
            assert "Failed to build the `buck2 dev` server" in error["error"], error[
                "error"
            ]
            await dev.quiet()
            assert_kept(dev, first)

            write(buck, "app/main.txt", "app v2\n")
            server = read(buck, "app/server.py")
            write(buck, "app/server.py", server + "# changed\n")
            await dev.wait_until_quiet()
            assert_kept(dev, first)

            write(buck, "app/check.txt", "ok\n")
            shutdown, started, _, _, built = await dev.expect(
                "shutdown", "started", "ready", "build", "built"
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
        """While the server's build is broken, a change to the server's script builds it again,
        which fails again: one more `error`."""
        dev = await start_dev(buck, "//app:built_server")
        try:
            first = await dev.start()
            write(buck, "app/check.txt", "broken\n")
            await dev.expect("error")
            await dev.quiet()

            server = read(buck, "app/server.py")
            write(buck, "app/server.py", server + "# changed\n")
            await dev.expect("error")
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_server_build_fails_while_the_output_builds(self, buck: Buck) -> None:
        """The server's own build broken: each change to an output builds the server again,
        which fails, so the server gets `error` again, and the old server keeps running. The
        output is still stale after a change, and builds when the server asks for it."""
        dev = await start_dev(buck, "//app:built_server")
        try:
            first = await dev.start()
            write(buck, "app/check.txt", "broken\n")
            (error,) = await dev.expect("error")
            assert "Failed to build the `buck2 dev` server" in error["error"]

            write(buck, "app/main.txt", "app v2\n")
            _, _, _, built = await dev.expect("error", "stale", "build", "built")
            assert site(built) == "app v2\n"

            # A change to the target does the same.
            edit_target(
                buck,
                APP,
                "built_server",
                'srcs = ["main.txt"],',
                'srcs = ["main.txt", "other.txt"],',
            )
            _, _, _, built = await dev.expect("error", "stale", "build", "built")
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
                'mode = "outputs",',
                'mode = "outputs",\n    server_exe = "no-such-program-for-buck2-dev",',
            )
            (shutdown,) = await dev.expect("shutdown")
            assert shutdown["pid"] == first["pid"]
            await dev.quiet()
            assert dev.running()
            assert not alive(first["pid"])

            write(buck, APP, targets)
            _, _, _, built = await dev.expect("started", "ready", "build", "built")
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
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            server = (buck.cwd / "app/server.py").read_text()
            write(buck, "app/server.py", "this is not python (\n" + server)
            (shutdown,) = await dev.expect("shutdown")
            assert shutdown["pid"] == first["pid"]
            await dev.quiet()
            assert dev.running()

            write(buck, "app/server.py", server)
            _, _, _, built = await dev.expect("started", "ready", "build", "built")
            assert site(built) == "app v1\n"
            await dev.quiet()
        finally:
            output = await dev.stop()
        assert "SyntaxError" in output, output
        assert "starts it again after the next change" in output, output

    @dev_test()
    async def test_restart_request_while_server_build_broken(self, buck: Buck) -> None:
        """The server asks for a restart while its own build is broken: `error`, the old server
        keeps running, and the fix restarts it."""
        dev = await start_dev(buck, "//app:built_server")
        try:
            first = await dev.start()
            write(buck, "app/check.txt", "broken\n")
            await dev.expect("error")

            dev.request({"type": "restart"})
            (error,) = await dev.expect("error")
            assert "Failed to build the `buck2 dev` server" in error["error"]
            await dev.quiet()
            assert_kept(dev, first)

            write(buck, "app/check.txt", "ok\n")
            shutdown, started = await dev.expect("shutdown", "started")
            assert shutdown["pid"] == first["pid"]
            # The new server gets `ready`, asks for `site`, and gets it.
            rest = await dev.collect()
            assert rest[0]["type"] == "ready", summary(rest)
            assert [e["type"] for e in rest[-2:]] == ["build", "built"], summary(rest)
            assert rest[-1]["output"] == "site"
            assert all(e["pid"] == started["pid"] for e in rest)
        finally:
            await dev.stop()

    @dev_test()
    async def test_build_request_while_analysis_broken(self, buck: Buck) -> None:
        """The server asks for a build while the analysis of the target fails: the failed
        analysis sends `error`, and the build that the server asks for gets one `failed` for its
        output, with the analysis error, and no second `error`."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            edit_target(buck, APP, "all", 'mode = "outputs"', 'mode = "bogus"')
            (error,) = await dev.expect("error")
            assert "unknown mode bogus" in error["error"], error["error"]

            await dev.quiet()
            dev.request({"type": "build", "outputs": ["site"]})
            _, failed = await dev.expect("build", "failed")
            assert failed["output"] == "site"
            assert "unknown mode bogus" in failed["error"], failed["error"]
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_build_request_for_unknown_output(self, buck: Buck) -> None:
        """The server asks for an output that `DevInfo` does not have: `failed` for it, and the
        server keeps running."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            dev.request({"type": "build", "outputs": ["nope"]})
            _, failed = await dev.expect("build", "failed")
            assert failed["output"] == "nope"
            assert "no output named `nope`" in failed["error"], failed["error"]
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_build_request_for_no_outputs(self, buck: Buck) -> None:
        """The server asks for a build of no outputs: there is nothing to answer, so nothing
        comes back, and the server keeps running."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            dev.request({"type": "build", "outputs": []})
            await dev.expect("build")
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()
