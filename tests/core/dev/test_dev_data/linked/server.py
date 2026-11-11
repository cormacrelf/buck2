# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# A `buck2 dev` server for `test_dev_linked`. It logs `started`, connects, asks for every output
# on `ready`, and on `built` runs each output: its `argv`, or else the file at its `path`. It
# appends each message it gets to `buck-out/dev_events.jsonl`, like `app/server.py`, with what
# each run printed and its exit code under `ran`, and the content of the output's file under
# `content`. Like `app/server.py`, it sends each line that the tests add to
# `buck-out/dev_control.jsonl` to `buck2 dev` as a request.

import json
import os
import socket
import subprocess
import threading
import time


def log(message):
    message["pid"] = os.getpid()
    with open(os.path.join("buck-out", "dev_events.jsonl"), "a") as f:
        f.write(json.dumps(message) + "\n")


def run(output):
    """Run an output or a sidecar as a server would: the files it needs must be there."""
    if "argv" in output:
        command = output["argv"]
    else:
        command = [output["path"]]
        with open(output["path"]) as f:
            output["content"] = f.read()
    ran = subprocess.run(command, capture_output=True, text=True)
    output["ran"] = {"code": ran.returncode, "stdout": ran.stdout, "stderr": ran.stderr}


CONTROL = os.path.join("buck-out", "dev_control.jsonl")


def control(request):
    """Send the lines that the tests add to `CONTROL` from now on."""
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
            request(json.loads(line))


def main():
    log({"type": "started"})
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(os.environ["BUCK2_DEV_SOCKET"])
    conn = sock.makefile("rwb")
    lock = threading.Lock()

    def request(message):
        with lock:
            conn.write((json.dumps(message) + "\n").encode())
            conn.flush()

    threading.Thread(target=control, args=(request,), daemon=True).start()
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
