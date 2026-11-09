# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# pyre-strict

# Tests for `buck2 dev`. The fixture's server (`app/server.py`) appends every message it gets
# to `dev_events.jsonl` in the project root; the tests read that file.

import asyncio
import json
import os
import signal
import time
from asyncio import subprocess
from pathlib import Path
from typing import Any, Dict, List

from buck2.tests.e2e_util.api.buck import Buck
from buck2.tests.e2e_util.asserts import expect_failure
from buck2.tests.e2e_util.buck_workspace import buck_test, BuckTestCase


class DevSession:
    def __init__(self, buck: Buck, process: subprocess.Process) -> None:
        self.buck = buck
        self.process = process
        self.events_path: Path = buck.cwd / "dev_events.jsonl"
        self.offset = 0

    def _events(self) -> List[Dict[str, Any]]:
        if not self.events_path.exists():
            return []
        lines = self.events_path.read_text().splitlines()
        return [json.loads(line) for line in lines if line.strip()]

    async def wait_for(self, kind: str, timeout: float = 90) -> Dict[str, Any]:
        """Wait for the next message of type `kind` after the ones already returned."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            events = self._events()
            for i in range(self.offset, len(events)):
                if events[i]["type"] == kind:
                    self.offset = i + 1
                    return events[i]
            if self.process.returncode is not None:
                raise AssertionError(
                    f"`buck2 dev` exited with {self.process.returncode} "
                    f"before sending `{kind}`"
                )
            await asyncio.sleep(0.1)
        output = await self.stop()
        raise AssertionError(
            f"Timed out waiting for `{kind}`: {self._events()}\n{output}"
        )

    async def assert_none(self, kind: str, seconds: float) -> None:
        """Assert that no message of type `kind` arrives within `seconds`."""
        await asyncio.sleep(seconds)
        later = [e for e in self._events()[self.offset :] if e["type"] == kind]
        assert later == [], later

    async def stop(self) -> str:
        """Stop `buck2 dev`, and return what it wrote to stdout and stderr."""
        if self.process.returncode is None:
            self.process.send_signal(signal.SIGINT)
        try:
            stdout, stderr = await asyncio.wait_for(
                self.process.communicate(), timeout=60
            )
        except (asyncio.TimeoutError, ValueError):
            self.process.kill()
            return "<buck2 dev did not exit>"
        out = (stdout or b"").decode(errors="replace")
        err = (stderr or b"").decode(errors="replace")
        return f"stdout:\n{out}\nstderr:\n{err}"


async def start_dev(buck: Buck, target: str) -> DevSession:
    process = await buck.run_buck_command(
        "dev", target, "--console", "none", "--debounce-ms", "200"
    ).start()
    return DevSession(buck, process)


def path(buck: Buck, rel: str) -> str:
    return os.path.realpath(buck.cwd / rel)


def site(built: Dict[str, Any]) -> str:
    return built["outputs"]["site"]["content"]


def write(buck: Buck, rel: str, content: str) -> None:
    target = buck.cwd / rel
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(content)


class DevTest(BuckTestCase):
    @buck_test()
    async def test_builds_when_a_watched_file_changes(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:auto")
        try:
            ready = await dev.wait_for("ready")
            assert ready["outputs"] == ["site"]
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

    @buck_test()
    async def test_notify_set_holds_builds_until_undone(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:auto")
        try:
            await dev.wait_for("built")

            # `lib` is in the `notify` set: the server is told, and nothing is built.
            write(buck, "lib/lib.txt", "lib v2\n")
            stale = await dev.wait_for("stale")
            assert [os.path.realpath(f) for f in stale["files"]] == [
                path(buck, "lib/lib.txt")
            ]

            # A change to the app's own file is held while `lib` differs from the last build.
            write(buck, "app/main.txt", "app v2\n")
            await dev.assert_none("building", 3)

            # Undoing the change to `lib` releases the held build.
            write(buck, "lib/lib.txt", "lib v1\n")
            stale = await dev.wait_for("stale")
            assert stale["files"] == []
            building = await dev.wait_for("building")
            assert [os.path.realpath(f) for f in building["files"]] == [
                path(buck, "app/main.txt")
            ]
            built = await dev.wait_for("built")
            assert site(built) == "lib v1\napp v2\n"
        finally:
            await dev.stop()

    @buck_test()
    async def test_server_build_request_clears_stale(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:auto_build_on_stale")
        try:
            await dev.wait_for("built")

            # The server answers `stale` by asking for a build, which uses the new `lib`.
            write(buck, "lib/lib.txt", "lib v2\n")
            stale = await dev.wait_for("stale")
            assert stale["files"] != []
            built = await dev.wait_for("built")
            assert site(built) == "lib v2\napp v1\n"
            stale = await dev.wait_for("stale")
            assert stale["files"] == []

            # Later changes to the app build again.
            write(buck, "app/main.txt", "app v2\n")
            built = await dev.wait_for("built")
            assert site(built) == "lib v2\napp v2\n"
        finally:
            await dev.stop()

    @buck_test()
    async def test_bzl_change_waits_with_a_notify_set(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:auto")
        try:
            await dev.wait_for("built")
            defs = (buck.cwd / "defs.bzl").read_text()

            # A `.bzl` file may change any target, so with a `notify` set it waits like a
            # file of that set: no resolve, which would build the server, and no build.
            write(buck, "defs.bzl", defs + "# changed\n")
            stale = await dev.wait_for("stale")
            assert [os.path.realpath(f) for f in stale["files"]] == [
                path(buck, "defs.bzl")
            ]
            await dev.assert_none("graph_changed", 3)
            await dev.assert_none("building", 0)

            # Undoing it clears `stale`, and the held resolve and build run.
            write(buck, "defs.bzl", defs)
            stale = await dev.wait_for("stale")
            assert stale["files"] == []
            await dev.wait_for("graph_changed")
            await dev.wait_for("built")
        finally:
            await dev.stop()

    @buck_test()
    async def test_unknown_request_is_reported(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:bad_request")
        try:
            await dev.wait_for("built")
        finally:
            output = await dev.stop()
        assert "ignored a request from the server" in output, output
        assert '"type": "bulid"' in output, output

    @buck_test()
    async def test_reconnects_after_the_daemon_restarts(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:all")
        try:
            await dev.wait_for("built")

            # Another client may kill or restart the daemon. buck2 dev connects again.
            await buck.kill()
            write(buck, "app/main.txt", "app v2\n")
            built = await dev.wait_for("built")
            assert site(built) == "app v2\n"
        finally:
            output = await dev.stop()
        assert "connecting again" in output, output

    @buck_test()
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

    @buck_test()
    async def test_restart_set_restarts_the_server(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:restart")
        try:
            first = await dev.wait_for("ready")
            await dev.wait_for("built")

            write(buck, "app/main.txt", "app v2\n")
            second = await dev.wait_for("ready")
            assert second["pid"] != first["pid"]
            built = await dev.wait_for("built")
            assert site(built) == "app v2\n"
        finally:
            await dev.stop()

    @buck_test()
    async def test_other_commands_do_not_wait_for_dev_builds(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:slow")
        try:
            await dev.wait_for("built")

            # Start a dev build that takes 60 seconds.
            write(buck, "app/delay.txt", "60\n")
            await dev.wait_for("building")
            await asyncio.sleep(2)

            # A build with different files preempts the dev build instead of waiting for it.
            write(buck, "app/delay.txt", "0\n")
            write(buck, "app/other.txt", "other v2\n")
            start = time.monotonic()
            await buck.build("//app:other")
            assert time.monotonic() - start < 30

            # buck2 dev retries, and its build succeeds.
            built = await dev.wait_for("built")
            assert site(built) == "app v1\n"
            assert all(
                e["type"] != "failed" for e in dev._events()
            ), dev._events()
        finally:
            await dev.stop()

    @buck_test()
    async def test_target_without_dev_info(self, buck: Buck) -> None:
        await expect_failure(
            buck.run_buck_command("dev", "//app:other"),
            stderr_regex="has no `DevInfo` provider",
        )

    @buck_test()
    async def test_dev_watch_rejects_unknown_output(self, buck: Buck) -> None:
        write(
            buck,
            "bad/defs.bzl",
            """
def _bad(ctx):
    out = ctx.actions.write("out.txt", "x")
    return [
        DefaultInfo(default_output = out),
        DevInfo(
            server = cmd_args("true"),
            outputs = {"out": out},
            watch = [DevWatch(targets = None, build = ["missing"])],
        ),
    ]

bad = rule(impl = _bad, attrs = {})
""",
        )
        write(
            buck,
            "bad/TARGETS.fixture",
            'load(":defs.bzl", "bad")\n\nbad(name = "bad")\n',
        )
        await expect_failure(
            buck.build("//bad:bad"),
            stderr_regex="builds output `missing`, which is not in `DevInfo.outputs`",
        )

