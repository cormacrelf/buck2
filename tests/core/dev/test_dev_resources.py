# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# pyre-strict

# Tests for `DevInfo.local_resources`, which the daemon sets up and holds for the session. The
# targets are in `resources/` of the fixture. Each resource's setup (`resources/setup.py`) starts
# a holder process, logs `resource_setup` with its process ID, and the holder logs
# `resource_stopped` when it gets `SIGTERM`. The server (`resources/server.py`) logs `started`
# with its environment. `//resources:app` has an output, `page`, that only `page.txt` builds.

import asyncio
import json
import os
import signal
from typing import Any, Dict, List

from buck2.tests.core.common.dev.dev_util import (
    alive,
    DevSession,
    dev_test,
    read,
    start_dev,
    wait_until,
    write,
)
from buck2.tests.e2e_util.api.buck import Buck
from buck2.tests.e2e_util.buck_workspace import BuckTestCase


def events(dev: DevSession, kind: str) -> List[Dict[str, Any]]:
    """Every event of type `kind` so far, whatever process logged it."""
    if not dev.events_path.exists():
        return []
    out = []
    for line in dev.events_path.read_text().splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event["type"] == kind:
            out.append(event)
    return out


def setups(dev: DevSession, name: str) -> List[Dict[str, Any]]:
    return [e for e in events(dev, "resource_setup") if e["name"] == name]


async def start_app(dev: DevSession) -> Dict[str, Any]:
    """Wait for the server of `//resources:app` to start, after both its resources."""
    started = await dev.wait_for("started")
    [db] = setups(dev, "db")
    [cache] = setups(dev, "cache")
    assert alive(db["pid"]) and alive(cache["pid"])
    return started


def edit(buck: Buck, rel: str) -> None:
    write(buck, rel, read(buck, rel) + "# edited\n")


class DevResourcesTest(BuckTestCase):
    @dev_test()
    async def test_resources_are_set_up_before_the_server(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//resources:app")
        try:
            started = await start_app(dev)
            [db] = setups(dev, "db")
            order = [
                e["type"]
                for e in map(json.loads, dev.events_path.read_text().splitlines())
            ]
            assert order == ["resource_setup", "resource_setup", "started"], order
            env = started["env"]
            assert env.get("DEV_DB_PID") == str(db["pid"]), env
            assert env.get("DEV_DB_VERSION") == "v1", env
            assert env.get("DEV_CACHE_PORT") == "5432", env
        finally:
            await dev.stop()

    @dev_test()
    async def test_resources_survive_a_server_restart(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//resources:app")
        try:
            first = await start_app(dev)
            edit(buck, "resources/server.py")
            second = await dev.wait_for("started")
            assert second["pid"] != first["pid"]
            assert len(setups(dev, "db")) == 1
            assert len(setups(dev, "cache")) == 1
            assert events(dev, "resource_stopped") == []
            assert second["env"]["DEV_DB_PID"] == first["env"]["DEV_DB_PID"]
        finally:
            await dev.stop()

    @dev_test()
    async def test_resources_are_stopped_when_buck2_dev_exits(
        self, buck: Buck
    ) -> None:
        dev = await start_dev(buck, "//resources:app")
        started = await start_app(dev)
        [db] = setups(dev, "db")
        [cache] = setups(dev, "cache")
        await dev.stop()
        assert not alive(started["pid"])
        stopped = {e["name"]: e["pid"] for e in events(dev, "resource_stopped")}
        assert stopped == {"db": db["pid"], "cache": cache["pid"]}, stopped
        # Another process reaps the holders.
        await wait_until(lambda: not alive(db["pid"]), "the db holder is gone")
        await wait_until(lambda: not alive(cache["pid"]), "the cache holder is gone")

    @dev_test()
    async def test_new_setup_restarts_the_resource_and_the_server(
        self, buck: Buck
    ) -> None:
        dev = await start_dev(buck, "//resources:app")
        try:
            first = await start_app(dev)
            [old] = setups(dev, "db")
            write(buck, "resources/db_version.txt", "v2\n")
            second = await dev.wait_for("started")
            [_, new] = setups(dev, "db")
            assert new["version"] == "v2"
            assert [e["pid"] for e in events(dev, "resource_stopped")] == [old["pid"]]
            # The server gets what the new resource gives it.
            assert second["env"]["DEV_DB_PID"] == str(new["pid"])
            assert second["env"]["DEV_DB_VERSION"] == "v2"
            assert not alive(first["pid"])
            # The other resource is left alone.
            assert len(setups(dev, "cache")) == 1
        finally:
            await dev.stop()

    @dev_test()
    async def test_new_setup_with_the_same_env_restarts_the_server(
        self, buck: Buck
    ) -> None:
        dev = await start_dev(buck, "//resources:app")
        try:
            first = await start_app(dev)
            [old] = setups(dev, "cache")
            write(buck, "resources/cache_version.txt", "v2\n")
            # The new cache gives the server the same port, but it is a new cache: the server
            # was started against the old one.
            second = await dev.wait_for("started")
            assert len(setups(dev, "cache")) == 2
            assert [e["pid"] for e in events(dev, "resource_stopped")] == [old["pid"]]
            assert second["env"]["DEV_CACHE_PORT"] == first["env"]["DEV_CACHE_PORT"]
            assert not alive(first["pid"])
            # The other resource is left alone.
            assert len(setups(dev, "db")) == 1
        finally:
            await dev.stop()

    @dev_test()
    async def test_resource_that_exits_is_set_up_again(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//resources:app")
        try:
            first = await start_app(dev)
            [db] = setups(dev, "db")
            os.kill(db["pid"], signal.SIGKILL)
            # Nothing notices until the next change.
            await asyncio.sleep(3)
            assert dev.running()
            assert alive(first["pid"])
            assert len(events(dev, "started")) == 1
            # A change that only builds an output sets the resource up again, and the server
            # starts again with the new one.
            write(buck, "resources/page.txt", "page v2\n")
            second = await dev.wait_for("started")
            [_, new] = setups(dev, "db")
            assert second["env"]["DEV_DB_PID"] == str(new["pid"])
            assert not alive(first["pid"])
            assert len(setups(dev, "cache")) == 1
        finally:
            output = await dev.stop()
        assert "Local resource `db` had exited" in output, output

    @dev_test()
    async def test_resources_are_released_when_the_client_is_killed(
        self, buck: Buck
    ) -> None:
        dev = await start_dev(buck, "//resources:app")
        first = await start_app(dev)
        [db] = setups(dev, "db")
        [cache] = setups(dev, "cache")
        try:
            # The daemon releases them when the session's file change subscription ends.
            dev.process.kill()
            await wait_until(
                lambda: not alive(db["pid"]) and not alive(cache["pid"]),
                "the daemon released the resources",
            )
            stopped = {e["name"] for e in events(dev, "resource_stopped")}
            assert stopped == {"db", "cache"}, stopped
        finally:
            # The server, in a process group of its own, outlives a client killed outright, and
            # holds the client's stderr open.
            if alive(first["pid"]):
                os.kill(first["pid"], signal.SIGKILL)
            await dev.stop()

    @dev_test()
    async def test_setup_that_fails(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//resources:app_broken")
        try:
            # No server without its resource, but the session goes on.
            await asyncio.sleep(3)
            assert dev.running()
            assert events(dev, "started") == []
            # Fixing the setup starts the resource and the server.
            write(buck, "resources/broken_version.txt", "v1\n")
            started = await dev.wait_for("started")
            [setup] = setups(dev, "broken")
            assert started["env"]["DEV_BROKEN_PORT"] == "5432"
            assert alive(setup["pid"])
        finally:
            output = await dev.stop()
        assert "could not set up local resource `broken`" in output, output
        assert "setup of broken says FAIL" in output, output

    @dev_test()
    async def test_setup_that_times_out(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//resources:app_slow")
        try:
            await wait_until(
                lambda: len(events(dev, "resource_slow")) == 1, "the setup starts"
            )
            [slow] = events(dev, "resource_slow")
            # The setup is killed after its timeout of 2 seconds, and the session goes on.
            await wait_until(lambda: not alive(slow["pid"]), "the setup is killed")
            await asyncio.sleep(1)
            assert dev.running()
            assert events(dev, "started") == []
        finally:
            output = await dev.stop()
        assert "could not set up local resource `slow`" in output, output
        assert "timed out" in output, output
