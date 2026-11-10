# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# A `buck2 dev` server for `test_dev_linked`. It logs `started`, connects, asks for every output
# on `ready`, and on `built` runs each output from where `built` says it is. It appends each
# message it gets to `buck-out/dev_events.jsonl`, like `app/server.py`, with what each run
# printed and its exit code under `ran`, and the content of the output's file under `content`.

import json
import os
import socket
import subprocess


def log(message):
    message["pid"] = os.getpid()
    with open(os.path.join("buck-out", "dev_events.jsonl"), "a") as f:
        f.write(json.dumps(message) + "\n")


def run(output):
    """Run an output or a sidecar as a server would: the files it needs must be there."""
    command = [output["path"]]
    with open(output["path"]) as f:
        output["content"] = f.read()
    ran = subprocess.run(command, capture_output=True, text=True)
    output["ran"] = {"code": ran.returncode, "stdout": ran.stdout, "stderr": ran.stderr}


def main():
    log({"type": "started"})
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(os.environ["BUCK2_DEV_SOCKET"])
    conn = sock.makefile("rwb")

    def request(message):
        conn.write((json.dumps(message) + "\n").encode())
        conn.flush()

    request({"type": "hello", "version": 1})
    for line in conn:
        message = json.loads(line)
        kind = message["type"]
        if kind == "built":
            for output in message["outputs"].values():
                for artifact in [output, *output.get("sidecars", {}).values()]:
                    run(artifact)
        log(message)
        if kind == "ready":
            request({"type": "build", "outputs": message["outputs"]})
        elif kind == "shutdown":
            return


if __name__ == "__main__":
    main()
