# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# pyre-strict

# Tests for `buck2 dev`. The fixture's server (`app/server.py`) connects to `BUCK2_DEV_SOCKET`
# and appends every message it gets to `buck-out/dev_events.jsonl`; the tests read that file.
# The helpers are in `tests/core/common/dev`, shared with the other tests here.

import asyncio
import os
import signal
import time

from buck2.tests.core.common.dev.dev_util import (
    alive,
    assert_kept,
    contents,
    dev_test,
    path,
    site,
    start_dev,
    stopped,
    wait_until,
    write,
)
from buck2.tests.e2e_util.api.buck import Buck
from buck2.tests.e2e_util.asserts import expect_failure
from buck2.tests.e2e_util.buck_workspace import buck_test, BuckTestCase


class DevTest(BuckTestCase):
    @dev_test()
    async def test_builds_when_a_watched_file_changes(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:with_dep")
        try:
            ready = await dev.wait_for("ready")
            assert ready["outputs"] == ["site"]
            assert ready["version"] == 1
            built = await dev.wait_for("built")
            assert site(built) == "lib v1\napp v1\n"

            write(buck, "app/main.txt", "app v2\n")
            building = await dev.wait_for("building")
            assert [os.path.realpath(f) for f in building["files"]] == [
                path(buck, "app/main.txt")
            ]
            built = await dev.wait_for("built")
            assert site(built) == "lib v1\napp v2\n"
            assert built["outputs"]["site"]["changed"]
            assert built["gen"] == building["gen"]
        finally:
            await dev.stop()

    @dev_test()
    async def test_unknown_request_is_reported(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:bad_request")
        try:
            await dev.wait_for("built")
        finally:
            output = await dev.stop()
        assert "ignored a request from the server" in output, output
        assert '"type": "bulid"' in output, output
        # The server's own output goes to the terminal.
        assert "server started" in output, output

    @dev_test()
    async def test_new_source_file_changes_the_graph(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:pages")
        try:
            built = await dev.wait_for("built")
            assert site(built) == "page a\n"

            # `glob` picks up the new file once buck2 dev resolves the target again.
            write(buck, "app/pages/b.txt", "page b\n")
            await dev.wait_for("graph_changed")
            built = await dev.wait_for("built")
            assert site(built) == "page a\npage b\n"
        finally:
            await dev.stop()

    @dev_test()
    async def test_restart_request_restarts_the_server(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.wait_for("ready")
            await dev.wait_for("built")

            dev.request({"type": "restart"})
            # The old server is asked to stop, and is gone before the new one starts.
            shutdown = await dev.wait_for("shutdown")
            assert shutdown["pid"] == first["pid"]
            second = await dev.wait_for("ready")
            assert second["pid"] != first["pid"]
            assert not alive(first["pid"])
            built = await dev.wait_for("built")
            assert site(built) == "app v1\n"
        finally:
            await dev.stop()
        # `buck2 dev` stops the server, and waits for it, before it exits.
        assert not alive(second["pid"])
        assert dev._events()[-1]["type"] == "shutdown", dev._events()

    @dev_test()
    async def test_other_commands_do_not_wait_for_dev_builds(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:slow")
        try:
            await dev.wait_for("built")

            # Start a dev build that takes 15 seconds.
            write(buck, "app/delay.txt", "15\n")
            await dev.wait_for("building")
            await asyncio.sleep(2)

            # A build with different files preempts the dev build instead of waiting for it.
            write(buck, "app/other.txt", "other v2\n")
            start = time.monotonic()
            await buck.build("//app:other")
            assert time.monotonic() - start < 30

            # buck2 dev retries. Make its build quick, and it succeeds.
            write(buck, "app/delay.txt", "0\n")
            built = await dev.wait_for("built")
            assert site(built) == "app v1\n"
            assert all(
                e["type"] != "failed" for e in dev._events()
            ), dev._events()
        finally:
            await dev.stop()

    @dev_test()
    async def test_change_during_a_build_cancels_it(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:slow")
        try:
            await dev.wait_for("built")

            # Start a dev build that takes 60 seconds.
            write(buck, "app/delay.txt", "60\n")
            first = await dev.wait_for("building")
            await asyncio.sleep(2)

            # A change to a file that the build builds from stops it, and the next build
            # starts once the files stop changing.
            start = time.monotonic()
            write(buck, "app/delay.txt", "0\n")
            write(buck, "app/main.txt", "app v2\n")
            cancelled = await dev.wait_for("cancelled")
            assert cancelled["gen"] == first["gen"]
            building = await dev.wait_for("building")
            assert path(buck, "app/main.txt") in [
                os.path.realpath(f) for f in building["files"]
            ]
            built = await dev.wait_for("built")
            assert built["gen"] == building["gen"]
            assert site(built) == "app v2\n"
            assert time.monotonic() - start < 30
        finally:
            await dev.stop()

    @dev_test()
    async def test_change_that_the_build_does_not_use_does_not_cancel_it(
        self, buck: Buck
    ) -> None:
        dev = await start_dev(buck, "//app:slow")
        try:
            await dev.wait_for("built")

            write(buck, "app/delay.txt", "4\n")
            first = await dev.wait_for("building")
            await asyncio.sleep(1)

            # A new file that no target uses, and a file of the build saved unchanged.
            write(buck, "app/unrelated.txt", "unrelated\n")
            os.utime(buck.cwd / "app/main.txt")
            built = await dev.wait_for("built")
            assert built["gen"] == first["gen"]

            # The build read `main.txt` as it is after the save, so the save is no reason to
            # build again after it.
            await dev.assert_none("building", 3)
            assert all(e["type"] != "cancelled" for e in dev._events()), dev._events()
        finally:
            await dev.stop()

    @dev_test()
    async def test_change_during_a_build_is_built_after_it(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:pages_slow")
        try:
            await dev.wait_for("built")

            write(buck, "app/delay.txt", "4\n")
            first = await dev.wait_for("building")
            await asyncio.sleep(1)

            # A new file that the build does not use yet: the build goes on, and the next one,
            # with the file, starts after it.
            write(buck, "app/pages/b.txt", "page b\n")
            built = await dev.wait_for("built")
            assert built["gen"] == first["gen"]
            await dev.wait_for("graph_changed")
            built = await dev.wait_for("built")
            assert built["gen"] > first["gen"]
            assert site(built) == "page a\npage b\n"
            assert all(e["type"] != "cancelled" for e in dev._events()), dev._events()
        finally:
            await dev.stop()

    @dev_test()
    async def test_new_server_command_restarts_the_server(self, buck: Buck) -> None:
        """The server's command names the server script, so new contents of the script start
        the server again, and a save of it without a change does not."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()

            # The same contents: nothing is built from the script, and the server keeps
            # running.
            os.utime(buck.cwd / "app/server.py")
            await dev.quiet()
            assert_kept(dev, first)

            # New contents: the old server is stopped, and nothing is built for it. The new
            # one asks for what it needs.
            server = (buck.cwd / "app/server.py").read_text()
            write(buck, "app/server.py", server + "# changed\n")
            shutdown, started, _ = await dev.expect("shutdown", "started", "ready")
            assert shutdown["pid"] == first["pid"]
            assert started["pid"] != first["pid"]
            building, built = await dev.expect("building", "built")
            assert building["files"] == []
            assert built["pid"] == started["pid"]
            await dev.quiet()

            # A file that the command does not name builds, and the server keeps running.
            write(buck, "app/main.txt", "app v2\n")
            _, built = await dev.expect("building", "built")
            assert site(built) == "app v2\n"
            assert built["pid"] == started["pid"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_server_is_read_during_a_build(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:slow_flood")
        try:
            await dev.wait_for("built")

            # Start a dev build that takes 10 seconds. The server sends a request far bigger
            # than a socket buffer when it starts, which it can only finish sending if
            # `buck2 dev` reads it during the build.
            write(buck, "app/delay.txt", "10\n")
            building = await dev.wait_for("building")
            await dev.wait_for("flooded", timeout=30)
            assert all(
                e.get("gen") != building["gen"]
                for e in dev._events()
                if e["type"] in ("built", "failed")
            ), dev._events()

            # The build finishes, and then the request is handled.
            built = await dev.wait_for("built")
            assert built["gen"] == building["gen"]
            second = await dev.wait_for("building")
            assert second["gen"] > building["gen"]
            await dev.wait_for("built")
        finally:
            await dev.stop()

    @dev_test()
    async def test_server_that_never_connects_is_restarted(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:no_connect")
        try:
            first = await dev.wait_for("started")

            server = (buck.cwd / "app/server.py").read_text()
            write(buck, "app/server.py", server + "# changed\n")
            second = await dev.wait_for("started")
            assert second["pid"] != first["pid"]
            assert not alive(first["pid"])
        finally:
            await dev.stop()
        assert not alive(second["pid"])

    @dev_test()
    async def test_ctrl_z_stops_the_server_too(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:no_connect")
        try:
            server = (await dev.wait_for("started"))["pid"]
            pid = dev.process.pid

            # Ctrl-Z reaches `buck2 dev`, which stops the server, then itself.
            dev.process.send_signal(signal.SIGTSTP)
            await wait_until(lambda: stopped(pid), "buck2 dev stops")
            assert stopped(server)

            # `fg` continues both.
            dev.process.send_signal(signal.SIGCONT)
            await wait_until(lambda: not stopped(server), "the server continues")
            assert not stopped(pid)

            # And `buck2 dev` still works.
            server_py = (buck.cwd / "app/server.py").read_text()
            write(buck, "app/server.py", server_py + "# changed\n")
            second = await dev.wait_for("started")
            assert second["pid"] != server
        finally:
            await dev.stop()

    @dev_test()
    async def test_crashed_server_starts_again_after_a_change(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:crash")
        try:
            first = await dev.wait_for("ready")
            await dev.wait_for("built")
            await dev.wait_for("crashing")

            # The server exited with an error. `buck2 dev` keeps running, and waits.
            await asyncio.sleep(2)
            assert dev.process.returncode is None
            await dev.assert_none("ready", 0)

            # The next change starts it again.
            write(buck, "app/main.txt", "app v2\n")
            second = await dev.wait_for("ready")
            assert second["pid"] != first["pid"]
            built = await dev.wait_for("built")
            assert site(built) == "app v2\n"
        finally:
            output = await dev.stop()
        assert "starts it again after the next change" in output, output

    @dev_test()
    async def test_server_that_exits_successfully_starts_again_after_a_change(
        self, buck: Buck
    ) -> None:
        dev = await start_dev(buck, "//app:run_once")
        try:
            first = await dev.wait_for("ready")
            await dev.wait_for("built")
            await dev.wait_for("exiting")

            # A server that runs to completion does not end the session.
            await asyncio.sleep(2)
            assert dev.process.returncode is None
            await dev.assert_none("ready", 0)

            write(buck, "app/main.txt", "app v2\n")
            second = await dev.wait_for("ready")
            assert second["pid"] != first["pid"]
            built = await dev.wait_for("built")
            assert site(built) == "app v2\n"
        finally:
            output = await dev.stop()
        assert "server exited. `buck2 dev` starts it again" in output, output

    @dev_test()
    async def test_files_that_cannot_change_the_graph_are_ignored(
        self, buck: Buck
    ) -> None:
        dev = await start_dev(buck, "//app:all")
        try:
            await dev.wait_for("built")

            # Created and removed at once, as vim does when it saves.
            temp = buck.cwd / "app/4913"
            temp.write_text("")
            temp.unlink()
            # `other.txt` belongs to a target outside the graph of `:all`. Writing to it adds no
            # file to the package.
            write(buck, "app/other.txt", "other v2\n")
            await dev.assert_none("graph_changed", 3)
            await dev.assert_none("building", 0)

            # A real change still builds.
            write(buck, "app/main.txt", "app v2\n")
            built = await dev.wait_for("built")
            assert site(built) == "app v2\n"
            await dev.assert_none("graph_changed", 0)
        finally:
            await dev.stop()

    @dev_test()
    async def test_too_long_line_closes_the_connection(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:too_long")
        try:
            await dev.wait_for("ready")
            await asyncio.sleep(2)
            # The server keeps running, but gets no more messages, and its build request,
            # sent after the long line, is not read.
            assert dev.process.returncode is None
            await dev.assert_none("built", 0)
        finally:
            output = await dev.stop()
        assert "longer than any request" in output, output

    @dev_test()
    async def test_superconsole_shows_the_server_output(self, buck: Buck) -> None:
        # `super` forces the superconsole, though stderr is not a terminal here.
        dev = await start_dev(buck, "//app:all", console="super")
        try:
            # The server writes to a pseudo-terminal.
            assert (await dev.wait_for("stdout"))["isatty"]
            await dev.wait_for("built")
            write(buck, "app/main.txt", "app v2\n")
            await dev.wait_for("built")
            await asyncio.sleep(1)
            assert dev.process.returncode is None
        finally:
            await dev.process_output_split()
        # `buck2 dev` shows it above the superconsole, on stderr.
        assert "server started" in dev.stderr, dev.stderr
        assert "server started" not in dev.stdout, dev.stdout

    @dev_test()
    async def test_simple_console_leaves_out_what_each_build_prints(
        self, buck: Buck
    ) -> None:
        dev = await start_dev(buck, "//app:all", console="simple")
        try:
            assert not (await dev.wait_for("stdout"))["isatty"]
            await dev.wait_for("built")
            write(buck, "app/main.txt", "app v2\n")
            await dev.wait_for("built")
        finally:
            output = await dev.stop()
        for chatter in ["Build ID:", "Cache hits", "Commands:", "File changed"]:
            assert chatter not in output, output
        # The server writes to stdout itself.
        assert "server started" in output, output

    @dev_test()
    async def test_server_and_output_take_their_own_files_of_the_target(
        self, buck: Buck
    ) -> None:
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.wait_for("ready")
            await dev.wait_for("built")

            # The server script and `main.txt` are both source files of `:all`. The server's
            # command names the script, and the site is built from `main.txt` only, so
            # `main.txt` only builds the site.
            write(buck, "app/main.txt", "app v2\n")
            built = await dev.wait_for("built")
            assert site(built) == "app v2\n"
            await dev.assert_none("ready", 0)

            server = (buck.cwd / "app/server.py").read_text()
            write(buck, "app/server.py", server + "# changed\n")
            second = await dev.wait_for("ready")
            assert second["pid"] != first["pid"]
        finally:
            await dev.stop()

    @dev_test()
    async def test_output_found_through_dynamic_actions_and_transitive_sets(
        self, buck: Buck
    ) -> None:
        dev = await start_dev(buck, "//app:deep")
        try:
            first = await dev.start()
            assert contents(first) == {"site": "lib v1\napp v1\n", "deep": "lib v1\n"}

            # `lib.txt` is only reachable from `deep` through a dynamic action and a
            # transitive set.
            write(buck, "lib/lib.txt", "lib v2\n")
            building, built = await dev.expect("building", "built")
            assert building["outputs"] == ["deep", "site"]
            assert contents(built) == {"site": "lib v2\napp v1\n", "deep": "lib v2\n"}

            # The app's own file is not below `deep`: it only builds the site.
            write(buck, "app/main.txt", "app v2\n")
            building, built = await dev.expect("building", "built")
            assert building["outputs"] == ["site"]
            assert site(built) == "lib v2\napp v2\n"
            assert built["pid"] == first["pid"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_build_file_change_acts_for_the_targets_it_changed(
        self, buck: Buck
    ) -> None:
        dev = await start_dev(buck, "//app:split")
        try:
            first = await dev.wait_for("ready")
            built = await dev.wait_for("built")
            assert site(built) == "part v1\napp v1\n"
            targets = (buck.cwd / "app/TARGETS.fixture").read_text()

            # The site is built from `:split`: changing it builds the site.
            targets = targets.replace(
                'name = "split",\n    mode = "outputs",\n    srcs = ["main.txt"],',
                'name = "split",\n    mode = "outputs",\n    srcs = ["main.txt", "other.txt"],',
            )
            write(buck, "app/TARGETS.fixture", targets)
            await dev.wait_for("graph_changed")
            built = await dev.wait_for("built")
            assert site(built) == "part v1\napp v1\nother v1\n"

            # And from `:part`, a dependency in the same build file.
            targets = targets.replace(
                'srcs = ["part.txt"]', 'srcs = ["part.txt", "main.txt"]'
            )
            write(buck, "app/TARGETS.fixture", targets)
            await dev.wait_for("graph_changed")
            built = await dev.wait_for("built")
            assert site(built) == "part v1\napp v1\napp v1\nother v1\n"
            await dev.assert_none("ready", 0)
            assert built["pid"] == first["pid"]
        finally:
            await dev.stop()

    @dev_test()
    async def test_failed_analysis_runs_again_after_any_change(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:built_server")
        try:
            await dev.wait_for("built")

            # Break the server's build. `buck2 dev` builds the server again after each
            # change, also to the build file.
            write(buck, "app/check.txt", "broken\n")
            await dev.wait_for("failed")
            targets = (buck.cwd / "app/TARGETS.fixture").read_text()
            write(buck, "app/TARGETS.fixture", targets + "# changed\n")
            await dev.wait_for("failed")

            # Fixing it does not change the graph, but the analysis runs again.
            write(buck, "app/check.txt", "ok\n")
            await dev.wait_for("graph_changed")
        finally:
            await dev.stop()

    @buck_test()
    async def test_needs_a_file_watcher_that_reports_changes(self, buck: Buck) -> None:
        await expect_failure(
            buck.run_buck_command("dev", "//app:all"),
            stderr_regex="The `fs_hash_crawler` file watcher cannot report file changes",
        )

    @dev_test()
    async def test_new_directory_in_a_package_is_watched(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:tree")
        try:
            built = await dev.wait_for("built")
            assert site(built) == "tree a\n"

            # The daemon watches the whole project, so a file in a directory that did not
            # exist when `buck2 dev` analysed the target is seen too.
            (buck.cwd / "app/tree/new").mkdir()
            # An empty directory changes no target.
            await dev.assert_none("graph_changed", 2)
            write(buck, "app/tree/new/b.txt", "tree b\n")
            await dev.wait_for("graph_changed")
            built = await dev.wait_for("built")
            assert site(built) == "tree a\ntree b\n"
        finally:
            await dev.stop()

    @dev_test()
    async def test_target_without_dev_info(self, buck: Buck) -> None:
        await expect_failure(
            buck.run_buck_command("dev", "//app:other"),
            stderr_regex="has no `DevInfo` provider",
        )
