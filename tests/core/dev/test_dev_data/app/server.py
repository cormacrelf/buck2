# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# A `buck2 dev` server for the tests. It connects to `BUCK2_DEV_SOCKET`, and appends each
# message it receives, with its process ID and the content of each built output, to
# `dev_events.jsonl` in the project root. It also logs some events of its own, whose types do
# not clash with `buck2 dev` messages.
#
# On `ready` it asks for the `site` output. With `--build-on-stale`, it asks for `site` again
# whenever `buck2 dev` reports files of a `notify` set as changed. With `--bad-request`, it also
# sends a request of an unknown type on `ready`. With `--no-connect`, it never connects, and only
# logs `started`. With `--crash-once`, the first server to get `built` exits with an error. With
# `--flood-on-second-build`, it sends one very long request when the second build starts, and
# logs `flooded` once `buck2 dev` has read it.

import json
import os
import socket
import sys
import time


def log(message):
    message["pid"] = os.getpid()
    if message["type"] == "built":
        for output in message["outputs"].values():
            with open(output["path"]) as f:
                output["content"] = f.read()
    with open("dev_events.jsonl", "a") as f:
        f.write(json.dumps(message) + "\n")


def connect():
    address = os.environ["BUCK2_DEV_SOCKET"]
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(address)
    return sock.makefile("rwb")


def main():
    args = sys.argv[1:]
    # Output goes to the terminal, like any other process's.
    print("server started", flush=True)
    if "--no-connect" in args:
        log({"type": "started"})
        while True:
            time.sleep(60)

    conn = connect()

    def request(message):
        conn.write((json.dumps(message) + "\n").encode())
        conn.flush()

    request({"type": "hello", "version": 1})
    builds = 0
    for line in conn:
        message = json.loads(line)
        log(message)
        kind = message["type"]
        if kind == "ready":
            if "--bad-request" in args:
                request({"type": "bulid", "outputs": ["site"]})
            request({"type": "build", "outputs": ["site"]})
        elif kind == "stale" and message["files"] and "--build-on-stale" in args:
            request({"type": "build", "outputs": ["site"]})
        elif kind == "building":
            builds += 1
            if builds == 2 and "--flood-on-second-build" in args:
                # Far more than a socket buffer: this returns only once `buck2 dev` reads it,
                # which it must do while it builds.
                request({"type": "build", "outputs": ["site"], "pad": "x" * (8 << 20)})
                log({"type": "flooded"})
        elif kind == "built" and "--crash-once" in args:
            if not os.path.exists("crashed"):
                open("crashed", "w").close()
                log({"type": "crashing"})
                sys.exit(3)
        elif kind == "shutdown":
            return


if __name__ == "__main__":
    main()
