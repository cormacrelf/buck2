# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

"""Runs the app, and sends it each patch that `buck2 dev` builds.

`buck2 dev` builds two outputs when this server asks for them:

- `exe`: the app's binary. This server starts a copy of it, and starts it again when it
  changes.
- `patch`: a shared library with the newest code of the patched crate, linked by ThinLink.

After a change, `buck2 dev` sends `stale`. If every change since the running binary was built
is in the scope `lib`, the patched crate, this server asks for a new `patch`. Otherwise
something else changed, which a patch cannot carry: it asks for both outputs, and starts the
app again.

For each new `patch`, this server writes a subsecond jump table to the app's stdin: the
address of each function in the running binary, and the address of the same function in
the patch. `thinlink jump-table` makes the table. The app loads the patch, and subsecond
then runs the new code.

The table is made here, not in a build action, because it must use the binary that is
running. Building `patch` builds the patched crate again, and Buck would build a new `exe`
with it if the patch depended on the binary.
"""

import argparse
import json
import os
import shutil
import signal
import socket
import subprocess
import sys

# Where this server keeps the running binary and each patch. Buck may replace its outputs, and
# the dynamic loader returns the library it already loaded for a path it has seen.
STATE = os.path.join("buck-out", "subsecond")


def log(message):
    sys.stderr.write("[subsecond] %s\n" % message)
    sys.stderr.flush()


def connect_to_buck2_dev():
    """Connect to `buck2 dev`. Returns a file to read and write JSON lines on."""
    address = os.environ["BUCK2_DEV_SOCKET"]
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(address)
    return sock.makefile("rwb")


def copy(src, name):
    os.makedirs(STATE, exist_ok=True)
    dst = os.path.abspath(os.path.join(STATE, name))
    shutil.copy2(src, dst)
    return dst


class App:
    def __init__(self, thinlink):
        self.thinlink = thinlink
        self.process = None
        self.exe = None
        self.runs = 0
        self.patches = 0

    def start(self, exe):
        self.stop()
        self.runs += 1
        self.exe = copy(exe, "app-%d" % self.runs)
        log("starting %s" % exe)
        # The app reads jump tables from its stdin.
        self.process = subprocess.Popen([self.exe], stdin=subprocess.PIPE, text=True)

    def stop(self):
        if self.process is not None and self.process.poll() is None:
            self.process.terminate()
            self.process.wait()
        self.process = None

    def patch(self, path):
        """Send the patch at `path` to the app. Returns False if the app needs a new binary."""
        self.patches += 1
        lib = copy(path, "patch-%d.so" % self.patches)
        table = json.loads(
            subprocess.run(
                [self.thinlink, "jump-table", self.exe, lib],
                check=True,
                capture_output=True,
                text=True,
            ).stdout
        )
        missing = table.pop("missing")
        unexported = table.pop("unexported")
        if missing or unexported:
            # The new code uses a symbol that the running binary does not have, for example
            # a generic function with new type arguments, or that it has but does not
            # export. A new binary has the new code, so the app runs it after a restart.
            if missing:
                log("the patch uses %d symbols the app does not have" % len(missing))
            if unexported:
                log(
                    "the patch uses symbols the app does not export: %s"
                    % ", ".join(unexported)
                )
            return False
        table["lib"] = lib
        self.process.stdin.write(json.dumps(table) + "\n")
        self.process.stdin.flush()
        log("sent patch %d with %d functions" % (self.patches, len(table["map"])))
        return True


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--thinlink", required=True)
    args = parser.parse_args()

    # `buck2 dev` may stop this server with SIGTERM. Exit through the `finally` below.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    app = App(os.path.abspath(args.thinlink))
    conn = connect_to_buck2_dev()

    def request(message):
        conn.write((json.dumps(message) + "\n").encode())
        conn.flush()

    # The outputs asked for that have no answer yet, and the answers so far. Each output comes
    # in a message of its own; this server acts once it has all of them.
    waiting = set()
    results = {}

    def build(outputs):
        waiting.update(outputs)
        request({"type": "build", "outputs": outputs})

    try:
        request({"type": "hello", "version": 2})
        for line in conn:
            message = json.loads(line)
            # Messages of other types may be added; ignore them.
            kind = message.get("type")
            if kind == "ready":
                build(["exe", "patch"])
            elif kind == "stale":
                # `exe`'s changes are those since the running binary was built, which is what
                # a patch is built against.
                exe = message["outputs"].get("exe", {})
                if "lib" in exe.get("within", []):
                    build(["patch"])
                else:
                    log("a change cannot be patched; building the app again")
                    build(["exe", "patch"])
            elif kind in ("built", "failed", "cancelled"):
                waiting.discard(message["output"])
                results[message["output"]] = message
                if waiting:
                    continue
                exe = results.pop("exe", None)
                patch = results.pop("patch", None)
                if any(r["type"] == "failed" for r in (exe, patch) if r is not None):
                    log("build failed; the app keeps running the last build")
                elif exe is not None and exe["type"] == "built":
                    if exe["changed"] or app.process is None:
                        app.start(exe["path"])
                elif patch is not None and patch["type"] == "built":
                    if patch["changed"] and app.process is not None:
                        if not app.patch(patch["path"]):
                            log("building the app again")
                            build(["exe", "patch"])
            elif kind == "error":
                log("build failed; the app keeps running the last build")
            elif kind == "shutdown":
                break
    finally:
        # `buck2 dev` is stopping this server, or has exited.
        app.stop()


if __name__ == "__main__":
    main()
