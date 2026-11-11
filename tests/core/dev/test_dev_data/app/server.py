# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# A `buck2 dev` server for the tests. It connects to `BUCK2_DEV_SOCKET`, and appends each
# message it receives, with its process ID and the content of each built output and sidecar, to
# `buck-out/dev_events.jsonl`, where the file watcher does not see it. It also logs some events of
# its own, whose types do not clash with `buck2 dev` messages. With `--log-in-project`, it also
# appends each to `server.log` in the project root, as a server that logs to the project would.
#
# On `ready` it asks for the `site` output. With `--bad-request`, it also sends a request of an
# unknown type on `ready`. With `--no-connect`, it never connects, and only logs `started`. With
# `--crash-once`, the first server to get `built` exits with an error. With `--exit-after-built`,
# every server exits successfully after `built`. With `--flood-on-second-build`, it sends one
# long request when the second build starts, and logs `flooded` once `buck2 dev` has read it.
# With `--too-long`, it sends a line longer than `buck2 dev` accepts on `ready`. With
# `--all-outputs`, it asks for every output that `ready` names instead of `site`. With
# `--slow-shutdown`, it takes 3 seconds to exit after `shutdown`.
#
# Every server logs `started` when it starts. The tests control the running server through
# `buck-out/dev_control.jsonl`, which the file watcher does not report: the server sends each line
# that is added to it to `buck2 dev` as a request, except that `{"type": "_exit", "code": N}`
# makes it exit with status N at once.

import json
import os
import socket
import sys
import threading
import time


def log(message):
    message["pid"] = os.getpid()
    if message["type"] == "built":
        for output in message["outputs"].values():
            for artifact in [output, *output.get("sidecars", {}).values()]:
                with open(artifact["path"]) as f:
                    artifact["content"] = f.read()
    line = json.dumps(message) + "\n"
    with open(os.path.join("buck-out", "dev_events.jsonl"), "a") as f:
        f.write(line)
    if "--log-in-project" in sys.argv:
        with open("server.log", "a") as f:
            f.write(line)


def connect():
    address = os.environ["BUCK2_DEV_SOCKET"]
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(address)
    return sock.makefile("rwb")


CONTROL = os.path.join("buck-out", "dev_control.jsonl")


def control(request):
    """Act on the lines that the tests add to `CONTROL` from now on."""
    try:
        offset = os.path.getsize(CONTROL)
    except OSError:
        offset = 0
    while True:
        time.sleep(0.05)
        try:
            with open(CONTROL, "rb") as f:
                f.seek(offset)
                data = f.read()
        except OSError:
            continue
        # Only whole lines.
        data = data[: data.rfind(b"\n") + 1]
        offset += len(data)
        for line in data.splitlines():
            message = json.loads(line)
            if message["type"] == "_exit":
                log({"type": "exiting", "code": message["code"]})
                os._exit(message["code"])
            request(message)


def main():
    args = sys.argv[1:]
    # Output goes to the terminal, like any other process's.
    print("server started", flush=True)
    log({"type": "started", "args": args})
    log({"type": "stdout", "isatty": sys.stdout.isatty()})
    if "--no-connect" in args:
        while True:
            time.sleep(60)

    conn = connect()
    lock = threading.Lock()

    def request(message):
        with lock:
            conn.write((json.dumps(message) + "\n").encode())
            conn.flush()

    threading.Thread(target=control, args=(request,), daemon=True).start()
    request({"type": "hello", "version": 1})
    builds = 0
    for line in conn:
        message = json.loads(line)
        log(message)
        kind = message["type"]
        if kind == "ready":
            if "--too-long" in args:
                request({"type": "build", "pad": "x" * (5 << 20)})
            if "--bad-request" in args:
                request({"type": "bulid", "outputs": ["site"]})
            if "--all-outputs" in args:
                request({"type": "build", "outputs": message["outputs"]})
            else:
                request({"type": "build", "outputs": ["site"]})
        elif kind == "building":
            builds += 1
            if builds == 2 and "--flood-on-second-build" in args:
                # Far more than a socket buffer, but less than the longest line `buck2 dev`
                # accepts: this returns only once `buck2 dev` reads it, which it must do while it
                # builds.
                request({"type": "build", "outputs": ["site"], "pad": "x" * (2 << 20)})
                log({"type": "flooded"})
        elif kind == "built" and "--exit-after-built" in args:
            log({"type": "exiting"})
            return
        elif kind == "built" and "--crash-once" in args:
            if not os.path.exists("crashed"):
                open("crashed", "w").close()
                log({"type": "crashing"})
                sys.exit(3)
        elif kind == "shutdown":
            if "--slow-shutdown" in args:
                time.sleep(3)
            return


if __name__ == "__main__":
    main()
