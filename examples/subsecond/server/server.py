# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

"""Runs the app, and sends it each patch that `buck2 dev` builds.

`buck2 dev` builds two outputs:

- `exe`: the app's binary. This server starts a copy of it, and starts it again when it
  changes.
- `patch`: a shared library with the newest code of the patched crate, linked by ThinLink.

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
import subprocess
import sys

# Where this server keeps the running binary and each patch. Buck may replace its outputs, and
# the dynamic loader returns the library it already loaded for a path it has seen.
STATE = os.path.join("buck-out", "subsecond")


def log(message):
    sys.stderr.write("[subsecond] %s\n" % message)
    sys.stderr.flush()


def request(message):
    """Send a request to `buck2 dev` on stdout."""
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


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
        # The app's stdout must not reach `buck2 dev`, which reads requests from ours.
        self.process = subprocess.Popen(
            [self.exe], stdin=subprocess.PIPE, stdout=sys.stderr, text=True
        )

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
        if missing:
            # The new code calls a function that the running binary does not have, for
            # example a generic function with new type arguments.
            log("the patch uses %d functions the app does not have" % len(missing))
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

    # `buck2 dev` sends SIGTERM when it exits. Exit through the `finally` below.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    app = App(os.path.abspath(args.thinlink))
    try:
        for line in sys.stdin:
            message = json.loads(line)
            kind = message["type"]
            if kind == "ready":
                request({"type": "build", "outputs": ["exe"]})
            elif kind == "built":
                outputs = message["outputs"]
                exe = outputs.get("exe")
                patch = outputs.get("patch")
                if exe is not None and (exe["changed"] or app.process is None):
                    app.start(exe["path"])
                elif patch is not None and patch["changed"] and app.process is not None:
                    if not app.patch(patch["path"]):
                        log("building the app again")
                        request({"type": "build", "outputs": ["exe"]})
            elif kind == "failed":
                log("build failed; the app keeps running the last build")
    finally:
        # `buck2 dev` closed stdin: it has exited.
        app.stop()


if __name__ == "__main__":
    main()
