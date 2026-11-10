# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

"""Time `buck2 dev` on a project that `gen.py` made.

usage: bench.py <buck2> <project> <mode>

`<buck2>` is the buck2 to time, such as the one `./bootstrap/buck2 build //:buck2` builds.
`<mode>` is a target of the project's `app` package: `targets`, `inputs` or `restart`.

It starts `buck2 dev`, with a daemon of its own, and prints how long these take:

- the start: until the server is `ready`, and from there until its first build is `built`;
- a change to a source file, three times: until the server gets `building`;
- a comment added to a build file: until the server gets `graph_changed`;
- every daemon command of the session, from its event log;
- the steps of finding what the watch sets claim, which the daemon logs.
"""

import json
import os
import signal
import subprocess
import sys
import time

buck2, project, mode = sys.argv[1:4]
project = os.path.abspath(project)
iso = "bench_" + mode
env = dict(os.environ, BUCK_LOG="buck2_server_commands::dev=debug")
# The project uses the prelude of the buck2 tests.
env.setdefault(
    "NANO_PRELUDE",
    os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "e2e_util", "nano_prelude"),
)
os.makedirs(os.path.join(project, "buck-out"), exist_ok=True)
events_path = os.path.join(project, "buck-out", "bench_events.jsonl")
if os.path.exists(events_path):
    os.unlink(events_path)


def events():
    """What the project's server logged: each message it got, with the time."""
    try:
        with open(events_path) as f:
            return [json.loads(line) for line in f if line.endswith("\n")]
    except OSError:
        return []


def wait_for(kind, after, timeout=1800):
    deadline = time.time() + timeout
    while time.time() < deadline:
        for e in events()[after:]:
            if e["type"] == kind:
                return e
        if dev.poll() is not None:
            raise SystemExit(f"buck2 dev exited with {dev.returncode}; see {output}")
        time.sleep(0.01)
    raise SystemExit(f"timed out waiting for `{kind}`")


output = os.path.join(project, "buck-out", f"dev_{mode}.out")
started = time.time()
dev = subprocess.Popen(
    [
        buck2,
        "--isolation-dir",
        iso,
        "dev",
        f"//app:{mode}",
        "--console",
        "none",
        "--debounce-ms",
        "20",
    ],
    cwd=project,
    env=env,
    stdout=open(output, "w"),
    stderr=subprocess.STDOUT,
)
ready = wait_for("ready", 0)
built = wait_for("built", 0)
print(f"start to `ready` (analysis, server build): {ready['at'] - started:.2f}s")
print(f"`ready` to first `built` (first Watch, first build): {built['at'] - ready['at']:.2f}s")
time.sleep(2)


def change(rel, text, expect):
    """Append `text` to a file, and return how long the server waited for `expect`."""
    seen = len(events())
    at = time.time()
    with open(os.path.join(project, rel), "a") as f:
        f.write(text)
    if expect is None:
        time.sleep(3)
        return None
    took = wait_for(expect, seen)["at"] - at
    if expect == "building":
        wait_for("built", seen)
    time.sleep(1)
    return took


packages = len(os.listdir(os.path.join(project, "pkg")))
for i in range(3):
    took = change(f"pkg/p{packages // 2 + i}/t0/s0.txt", "edit\n", "building")
    print(f"source edit to `building` (20ms of it waiting for more changes): {took * 1000:.0f}ms")
# A file that no target uses, in a package of the graph and outside them: no message.
change("pkg/p1/notes.md", "notes\n", None)
os.makedirs(os.path.join(project, "elsewhere"), exist_ok=True)
change("elsewhere/notes.md", "notes\n", None)
took = change(f"pkg/p{packages // 2}/BUCK", "# a comment\n", "graph_changed")
print(f"build file comment to `graph_changed`: {took * 1000:.0f}ms")
took = change(f"pkg/p{packages // 2}/t0/s0.txt", "edit\n", "building")
print(f"source edit after that to `building`: {took * 1000:.0f}ms")

dev.send_signal(signal.SIGINT)
dev.wait(timeout=120)

# The daemon commands of the session, in order, from its event log.
log = subprocess.run(
    [buck2, "--isolation-dir", iso, "log", "show"],
    cwd=project,
    env=env,
    capture_output=True,
    text=True,
)
durations = []
for line in log.stdout.splitlines():
    try:
        event = json.loads(line).get("Event")
    except ValueError:
        continue
    end = event and event["data"].get("SpanEnd")
    if end and "Command" in end["data"]:
        durations.append(end["duration_us"] / 1000)
print("daemon commands of the session:")
print("  " + "  ".join(f"{d:.0f}ms" for d in durations))
if log.returncode != 0:
    print(log.stderr[-2000:])
subprocess.run([buck2, "--isolation-dir", iso, "kill"], cwd=project, env=env, capture_output=True)

# What the daemon logged about finding the claims.
stderr = os.path.join(os.path.expanduser("~/.buck/buckd"), project.lstrip("/"), iso, "buckd.stderr")
print("daemon steps:")
try:
    with open(stderr) as f:
        for line in f:
            if "buck2 dev: " in line:
                print("  " + line[line.index("buck2 dev: ") + len("buck2 dev: ") :].rstrip())
except OSError as e:
    print(f"  no daemon log: {e}")
