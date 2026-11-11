# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# pyre-strict

# Helpers for the `buck2 dev` tests in `tests/core/dev`. They all use the fixture
# `tests/core/dev/test_dev_data`, whose server (`app/server.py`) connects to `BUCK2_DEV_SOCKET` and
# appends every message it gets to `buck-out/dev_events.jsonl`, which the file watcher does not
# report; the tests read that file.

import asyncio
import json
import os
import signal
import time
from asyncio import subprocess
from pathlib import Path
from typing import Any, Dict, List, Optional

from buck2.tests.e2e_util.api.buck import Buck
from buck2.tests.e2e_util.buck_workspace import buck_test

# The messages that `buck2 dev` sends the server.
PROTOCOL = {
    "ready",
    "building",
    "built",
    "failed",
    "cancelled",
    "stale",
    "graph_changed",
    "shutdown",
    "unsupported",
}

# What `DevSession.next` and `DevSession.expect` see: the messages, and `started`, which each
# server logs when it starts.
OBSERVED = PROTOCOL | {"started"}


class DevSession:
    def __init__(self, buck: Buck, process: subprocess.Process) -> None:
        self.buck = buck
        self.process = process
        self.events_path: Path = buck.cwd / "buck-out" / "dev_events.jsonl"
        self.offset = 0

    def _events(self) -> List[Dict[str, Any]]:
        if not self.events_path.exists():
            return []
        lines = self.events_path.read_text().splitlines()
        events = []
        for line in lines:
            if not line.strip():
                continue
            try:
                events.append(json.loads(line))
            except json.JSONDecodeError:
                # A line that the server is still writing.
                break
        return events

    def observed(self) -> List[Dict[str, Any]]:
        """Every message the servers got, and every server start, so far."""
        return [e for e in self._events() if e["type"] in OBSERVED]

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
                output = await self.stop()
                raise AssertionError(
                    f"`buck2 dev` exited with {self.process.returncode} "
                    f"before sending `{kind}`\n{output}"
                )
            await asyncio.sleep(0.1)
        output = await self.stop()
        raise AssertionError(
            f"Timed out waiting for `{kind}`: {self._events()}\n{output}"
        )

    async def _next_until(self, deadline: float) -> Optional[Dict[str, Any]]:
        """The next message or server start after the ones already returned, or `None` if none
        arrives before `deadline`. Other events that the server logs are skipped."""
        while True:
            events = self._events()
            for i in range(self.offset, len(events)):
                self.offset = i + 1
                if events[i]["type"] in OBSERVED:
                    return events[i]
            if time.monotonic() >= deadline:
                return None
            await asyncio.sleep(0.05)

    async def next(self, timeout: float = 90) -> Dict[str, Any]:
        """The next message or server start after the ones already returned."""
        event = await self._next_until(time.monotonic() + timeout)
        if event is None:
            output = await self.stop()
            raise AssertionError(
                f"Expected a message within {timeout}s, got none\n{output}"
            )
        return event

    async def expect(self, *kinds: str, timeout: float = 90) -> List[Dict[str, Any]]:
        """Assert that the next messages and server starts are of types `kinds`, in this order,
        all within `timeout` seconds, and return them. On failure, the message says what
        arrived instead."""
        deadline = time.monotonic() + timeout
        got = []
        for kind in kinds:
            event = await self._next_until(deadline)
            if event is not None:
                got.append(event)
            if event is None or event["type"] != kind:
                # Show a little of what follows, too.
                await asyncio.sleep(1)
                got += [
                    e for e in self._events()[self.offset :] if e["type"] in OBSERVED
                ]
                output = await self.stop()
                raise AssertionError(
                    f"Expected {list(kinds)} within {timeout}s, got {summary(got)}\n{output}"
                )
        return got

    async def quiet(self, seconds: float = 3) -> None:
        """Assert that no message arrives and no server starts within `seconds`."""
        await asyncio.sleep(seconds)
        later = [e for e in self._events()[self.offset :] if e["type"] in OBSERVED]
        if later:
            output = await self.stop()
            raise AssertionError(
                f"Expected nothing more, got {summary(later)}\n{output}"
            )

    async def wait_until_quiet(self, seconds: float = 2, timeout: float = 60) -> None:
        """Skip what arrives until nothing has for `seconds`."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            count = len(self._events())
            await asyncio.sleep(seconds)
            if len(self._events()) == count:
                self.offset = count
                return
        output = await self.stop()
        raise AssertionError(f"Messages kept coming\n{output}")

    async def collect(self, seconds: float = 3) -> List[Dict[str, Any]]:
        """The messages and server starts that arrive until nothing has for `seconds`."""
        start = self.offset
        await self.wait_until_quiet(seconds)
        return [e for e in self._events()[start:] if e["type"] in OBSERVED]

    async def assert_none(self, kind: str, seconds: float) -> None:
        """Assert that no message of type `kind` arrives within `seconds`."""
        await asyncio.sleep(seconds)
        later = [e for e in self._events()[self.offset :] if e["type"] == kind]
        assert later == [], later

    async def start(self, outputs: int = 1) -> Dict[str, Any]:
        """Wait for the server to start, connect, and get its first build. Returns the `built`
        message, which has the server's `pid`."""
        _, ready, building, built = await self.expect(
            "started", "ready", "building", "built"
        )
        assert ready["pid"] == built["pid"]
        assert building["files"] == []
        return built

    def request(self, message: Dict[str, Any]) -> None:
        """Have the running server send `message` to `buck2 dev`, or, for a type starting with
        `_`, do something itself (see `app/server.py`). The file is under `buck-out`, so the file
        watcher does not report it."""
        control = self.buck.cwd / "buck-out" / "dev_control.jsonl"
        control.parent.mkdir(parents=True, exist_ok=True)
        with open(control, "a") as f:
            f.write(json.dumps(message) + "\n")

    def running(self) -> bool:
        """Whether `buck2 dev` is still running."""
        return self.process.returncode is None

    async def process_output_split(self) -> None:
        """Stop `buck2 dev`, and keep its stdout and stderr apart in `self.stdout` and
        `self.stderr`."""
        if self.process.returncode is None:
            self.process.send_signal(signal.SIGINT)
        stdout, stderr = await asyncio.wait_for(self.process.communicate(), timeout=60)
        self.stdout: str = (stdout or b"").decode(errors="replace")
        self.stderr: str = (stderr or b"").decode(errors="replace")

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


def summary(events: List[Dict[str, Any]]) -> List[str]:
    """The events, short, for assertion messages."""
    out = []
    for e in events:
        detail = {k: v for k, v in e.items() if k in ("gen", "files", "error", "pid")}
        if e["type"] == "built":
            detail["outputs"] = {
                name: output.get("content") for name, output in e["outputs"].items()
            }
        elif "outputs" in e:
            detail["outputs"] = e["outputs"]
        if "error" in detail:
            detail["error"] = str(detail["error"])[:200]
        out.append(f"{e['type']} {detail}")
    return out


async def start_dev(
    buck: Buck,
    target: str,
    console: str = "none",
    *args: str,
    env: Optional[Dict[str, str]] = None,
) -> DevSession:
    """Start `buck2 dev`. `env` adds to the environment of the `buck2 dev` client."""
    process = await buck.run_buck_command(
        "dev", target, "--console", console, "--debounce-ms", "200", *args, env=env
    ).start()
    return DevSession(buck, process)


def path(buck: Buck, rel: str) -> str:
    return os.path.realpath(buck.cwd / rel)


def files(buck: Buck, message: Dict[str, Any]) -> List[str]:
    """The `files` of a `building` or `stale` message, relative to the project root."""
    root = os.path.realpath(buck.cwd)
    return sorted(os.path.relpath(os.path.realpath(f), root) for f in message["files"])


def site(built: Dict[str, Any]) -> str:
    return built["outputs"]["site"]["content"]


def contents(built: Dict[str, Any]) -> Dict[str, Optional[str]]:
    """The content of each output of a `built` message."""
    return {name: output.get("content") for name, output in built["outputs"].items()}


def changed(built: Dict[str, Any]) -> Dict[str, bool]:
    """Whether each output of a `built` message changed."""
    return {name: output["changed"] for name, output in built["outputs"].items()}


def write(buck: Buck, rel: str, content: str) -> None:
    target = buck.cwd / rel
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(content)


def read(buck: Buck, rel: str) -> str:
    return (buck.cwd / rel).read_text()


def replace(buck: Buck, rel: str, old: str, new: str) -> str:
    """Replace `old`, which must be there, with `new` in a file. Returns the old content."""
    content = read(buck, rel)
    assert old in content, (old, content)
    write(buck, rel, content.replace(old, new, 1))
    return content


def _target_block(content: str, name: str) -> "tuple[int, int]":
    """Where the call that defines target `name` is in a build file."""
    start = content.index(f'    name = "{name}",\n')
    start = content.rindex("\n\n", 0, start) + 2
    end = content.index("\n)\n", start) + 3
    return start, end


def edit_target(buck: Buck, rel: str, name: str, old: str, new: str) -> str:
    """Replace `old` with `new` in the definition of target `name` in the build file `rel`.
    Returns the old content of the build file."""
    content = read(buck, rel)
    start, end = _target_block(content, name)
    block = content[start:end]
    assert old in block, (old, block)
    write(buck, rel, content[:start] + block.replace(old, new, 1) + content[end:])
    return content


def remove_target(buck: Buck, rel: str, name: str) -> str:
    """Remove the definition of target `name` from the build file `rel`. Returns the old
    content of the build file."""
    content = read(buck, rel)
    start, end = _target_block(content, name)
    write(buck, rel, content[:start] + content[end:])
    return content


def stopped(pid: int) -> bool:
    """Whether the process is stopped, as by Ctrl-Z. Linux only."""
    stat = Path(f"/proc/{pid}/stat").read_text()
    return stat.rsplit(")", 1)[1].split()[0] == "T"


async def wait_until(condition: Any, what: str, timeout: float = 30) -> None:
    deadline = time.monotonic() + timeout
    while not condition():
        if time.monotonic() > deadline:
            raise AssertionError(f"Timed out waiting until {what}")
        await asyncio.sleep(0.1)


def alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


def dev_test(extra_buck_config: Optional[Dict[str, Dict[str, str]]] = None) -> Any:
    """`buck2 dev` needs a file watcher that reports changes as they happen. The tests use
    `fs_hash_crawler` otherwise."""
    config = {"buck2": {"file_watcher": "notify"}}
    for section, values in (extra_buck_config or {}).items():
        config.setdefault(section, {}).update(values)
    return buck_test(extra_buck_config=config)


def assert_kept(dev: DevSession, first: Dict[str, Any]) -> None:
    """Assert that `buck2 dev` still runs, and so does the server that sent `first`."""
    assert dev.running(), "`buck2 dev` exited"
    assert alive(first["pid"]), "the server exited"


APP = "app/TARGETS.fixture"
LIB = "lib/TARGETS.fixture"
