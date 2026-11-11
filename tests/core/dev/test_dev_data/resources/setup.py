# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# The setup command of a local resource: `setup.py NAME VERSION_FILE`. It starts a process that
# holds the resource, logs `resource_setup` to `buck-out/dev_events.jsonl`, prints the JSON of a
# local resource, and exits. The holder logs `resource_stopped` when it gets `SIGTERM`.
#
# If the version file says `FAIL`, the setup fails instead. If it says `SLOW`, the setup logs
# `resource_slow` with its process ID, and sleeps for a minute.

import json
import os
import signal
import subprocess
import sys
import time

EVENTS = os.path.join("buck-out", "dev_events.jsonl")


def log(message):
    with open(EVENTS, "a") as f:
        f.write(json.dumps(message) + "\n")


def hold(name):
    stopped = False

    def on_term(_signum, _frame):
        nonlocal stopped
        if not stopped:
            stopped = True
            log({"type": "resource_stopped", "name": name, "pid": os.getpid()})
        sys.exit(0)

    signal.signal(signal.SIGTERM, on_term)
    while True:
        time.sleep(60)


def main():
    if sys.argv[1] == "--hold":
        hold(sys.argv[2])
        return
    name = sys.argv[1]
    with open(sys.argv[2]) as f:
        version = f.read().strip()
    if version == "FAIL":
        print(f"setup of {name} says FAIL", file=sys.stderr)
        sys.exit(3)
    if version == "SLOW":
        log({"type": "resource_slow", "name": name, "pid": os.getpid()})
        time.sleep(60)
    holder = subprocess.Popen(
        [sys.executable, __file__, "--hold", name],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    log(
        {
            "type": "resource_setup",
            "name": name,
            "pid": holder.pid,
            "version": version,
        }
    )
    print(
        json.dumps(
            {
                "pid": holder.pid,
                "resources": [
                    {"pid": str(holder.pid), "port": "5432", "version": version}
                ],
            }
        )
    )


main()
