# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# pyre-strict

# What `buck2 dev` does when things happen at awkward moments: right after it starts, during a
# build, during a restart, while the server has exited, and when the daemon goes away or another
# command runs. Each test states the behaviour it requires. Some fail against the current
# `buck2 dev`: they describe behaviour it should have. They use the fixture of `test_dev`.

import asyncio

from buck2.tests.core.common.dev.dev_util import (
    alive,
    APP,
    assert_kept,
    dev_test,
    edit_target,
    files,
    read,
    site,
    start_dev,
    write,
)
from buck2.tests.e2e_util.api.buck import Buck
from buck2.tests.e2e_util.buck_workspace import BuckTestCase


class DevTimingTest(BuckTestCase):
    @dev_test()
    async def test_change_right_after_start(self, buck: Buck) -> None:
        """A change as `buck2 dev` starts: one server, and it ends up with the change built."""
        dev = await start_dev(buck, "//app:all")
        write(buck, "app/main.txt", "app v2\n")
        try:
            started = await dev.wait_for("started")
            # Whether the change comes before the server's first build or after it depends on
            # timing; either way the last build has it.
            built = await dev.wait_for("built")
            while site(built) != "app v2\n":
                built = await dev.wait_for("built")
            await dev.quiet()
            assert built["pid"] == started["pid"]
            assert [e["type"] for e in dev.observed()].count("started") == 1
            assert all(e["type"] not in ("failed", "error") for e in dev.observed()), (
                dev.observed()
            )
        finally:
            await dev.stop()

    @dev_test()
    async def test_build_file_change_during_a_build(self, buck: Buck) -> None:
        """A change to the build file during a build stops it: `cancelled`, `graph_changed`,
        `stale`, and the server's build of the new graph."""
        write(buck, "app/zero.txt", "0\n")
        dev = await start_dev(buck, "//app:slow")
        try:
            first = await dev.start()
            write(buck, "app/delay.txt", "30\n")
            await dev.expect("stale", "build")
            await asyncio.sleep(1)

            # The new graph also builds without the delay.
            edit_target(
                buck,
                APP,
                "slow",
                'srcs = ["main.txt"],\n    delay = "delay.txt",',
                'srcs = ["main.txt", "other.txt"],\n    delay = "zero.txt",',
            )
            cancelled, _, stale, _, built = await dev.expect(
                "cancelled", "graph_changed", "stale", "build", "built"
            )
            assert cancelled["output"] == "site"
            # The cancelled build did not clear the change to `delay.txt`.
            assert files(buck, stale["outputs"]["site"]) == [APP, "app/delay.txt"], stale
            assert site(built) == "app v1\nother v1\n"
            assert built["pid"] == first["pid"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_breaking_change_during_a_build(self, buck: Buck) -> None:
        """A change that breaks the build, during a build: `cancelled`, `stale`, then a build
        that fails. The server keeps running."""
        dev = await start_dev(buck, "//app:slow")
        try:
            first = await dev.start()
            write(buck, "app/delay.txt", "30\n")
            await dev.expect("stale", "build")
            await asyncio.sleep(1)

            write(buck, "app/delay.txt", "0\n")
            write(buck, "app/main.txt", "FAIL v2\n")
            _, stale, _, _ = await dev.expect("cancelled", "stale", "build", "failed")
            assert files(buck, stale["outputs"]["site"]) == [
                "app/delay.txt",
                "app/main.txt",
            ]
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_change_while_the_daemon_says_what_changed(self, buck: Buck) -> None:
        """A change while the daemon takes its time to say what changed, because finding the
        files to watch builds what a dynamic action needs: it is handled afterwards. No output
        is built from `watch_delay.txt` itself."""
        dev = await start_dev(buck, "//app:slow_watch")
        try:
            first = await dev.start()
            write(buck, "app/watch_delay.txt", "3\n")
            await asyncio.sleep(1.5)
            write(buck, "app/main.txt", "app v2\n")
            stale, build, built = await dev.expect("stale", "build", "built")
            assert build["outputs"] == ["site"]
            assert files(buck, stale["outputs"]["site"]) == ["app/main.txt"]
            assert site(built) == "lib v1\napp v2\n"
            assert built["pid"] == first["pid"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_build_request_during_a_build(self, buck: Buck) -> None:
        """The server asks for the same output again while it builds: it is not built twice,
        and one `built` answers both requests."""
        dev = await start_dev(buck, "//app:slow")
        try:
            await dev.start()
            write(buck, "app/delay.txt", "3\n")
            await dev.expect("stale", "build")
            dev.request({"type": "build", "outputs": ["site"]})
            second, built = await dev.expect("build", "built")
            assert second["outputs"] == ["site"]
            assert built["output"] == "site"
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_restart_request_during_a_build(self, buck: Buck) -> None:
        """The server asks for a restart during a build: the build finishes, and then the server
        restarts and asks for its output."""
        dev = await start_dev(buck, "//app:slow")
        try:
            first = await dev.start()
            write(buck, "app/delay.txt", "3\n")
            await dev.expect("stale", "build")
            dev.request({"type": "restart"})
            built, shutdown, started, _, _, built2 = await dev.expect(
                "built", "shutdown", "started", "ready", "build", "built"
            )
            assert built["pid"] == first["pid"]
            assert shutdown["pid"] == first["pid"]
            assert built2["pid"] == started["pid"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_change_during_a_server_restart(self, buck: Buck) -> None:
        """A change to the server's script while the old server takes its time to stop: the
        new server is built after the change, so it is not restarted again for it."""
        dev = await start_dev(buck, "//app:slow_shutdown")
        try:
            first = await dev.start()
            server = read(buck, "app/server.py")
            write(buck, "app/server.py", server + "# v2\n")
            (shutdown,) = await dev.expect("shutdown")
            assert shutdown["pid"] == first["pid"]
            # The old server takes 3 seconds to exit.
            write(buck, "app/server.py", server + "# v3\n")
            started, _, _, built = await dev.expect(
                "started", "ready", "build", "built", timeout=15
            )
            assert built["pid"] == started["pid"]
            await dev.quiet(5)
        finally:
            await dev.stop()

    @dev_test()
    async def test_build_file_change_while_server_exited(self, buck: Buck) -> None:
        """The server exited, then the build file changed: the server starts again."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            dev.request({"type": "_exit", "code": 0})
            await dev.wait_for("exiting")
            await dev.quiet(2)

            edit_target(
                buck,
                APP,
                "all",
                'srcs = ["main.txt"],',
                'srcs = ["main.txt", "other.txt"],',
            )
            started, _, _, built = await dev.expect("started", "ready", "build", "built")
            assert started["pid"] != first["pid"]
            assert site(built) == "app v1\nother v1\n"
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_breaking_change_while_server_exited(self, buck: Buck) -> None:
        """The server exited, then a change broke the build: the server starts again, and its
        build fails."""
        dev = await start_dev(buck, "//app:all")
        try:
            await dev.start()
            dev.request({"type": "_exit", "code": 1})
            await dev.wait_for("exiting")
            write(buck, "app/main.txt", "FAIL v2\n")
            started, _, _, _ = await dev.expect("started", "ready", "build", "failed")
            await dev.quiet()
            assert alive(started["pid"])
        finally:
            await dev.stop()

    @dev_test()
    async def test_broken_analysis_while_server_exited(self, buck: Buck) -> None:
        """The server exited, then the analysis broke: nothing starts, `buck2 dev` keeps running,
        and the fix starts the server."""
        dev = await start_dev(buck, "//app:all")
        try:
            await dev.start()
            dev.request({"type": "_exit", "code": 1})
            await dev.wait_for("exiting")
            targets = edit_target(buck, APP, "all", 'mode = "outputs"', 'mode = "bogus"')
            await dev.quiet()
            assert dev.running()

            write(buck, APP, targets)
            await dev.expect("started", "ready", "build", "built")
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_other_build_of_the_same_files_during_a_dev_build(
        self, buck: Buck
    ) -> None:
        """Another `buck2 build` with the same files during a dev build: neither stops the
        other."""
        dev = await start_dev(buck, "//app:slow")
        try:
            await dev.start()
            write(buck, "app/delay.txt", "3\n")
            await dev.expect("stale", "build")
            await buck.build("//app:slow")
            (built,) = await dev.expect("built")
            assert built["output"] == "site"
            await dev.quiet()
        finally:
            await dev.stop()
