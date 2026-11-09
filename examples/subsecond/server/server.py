# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

"""Runs the app, and sends it each patch that `buck2 dev` builds.

`buck2 dev` builds two outputs:

- `exe`: the app's binary. This server starts it, and starts it again when it changes.
- `patch`: a shared library with the newest code of the patched crate.

For each new `patch`, this server writes a subsecond jump table to the app's stdin: the
address of each function in the running binary, and the address of the same function in
the patch. The app loads the patch, and `subsecond::call` then runs the new code.

The table is made here, not in a build action, because it must use the binary that is
running. Building `patch` builds the patched crate again, and Buck would build a new `exe`
with it if the patch depended on the binary.
"""

import json
import os
import shutil
import signal
import subprocess
import sys

# Where this server keeps a copy of each patch. The dynamic loader returns the library it
# already loaded for a path it has seen, so each patch needs a new path.
PATCHES = os.path.join("buck-out", "subsecond")


def log(message):
    sys.stderr.write("[subsecond] %s\n" % message)
    sys.stderr.flush()


def request(message):
    """Send a request to `buck2 dev` on stdout."""
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def symbols(path, *flags):
    """The symbols that `nm` lists for `path`, as name to (type, address)."""
    out = subprocess.run(
        ["nm", "--format=posix", *flags, path],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    result = {}
    for line in out.splitlines():
        parts = line.split()
        if len(parts) >= 3:
            result[parts[0]] = (parts[1], int(parts[2], 16))
        elif len(parts) == 2:
            result[parts[0]] = (parts[1], None)
    return result


def is_rust(name):
    return name.startswith("_R") or name.startswith("_ZN")


class App:
    def __init__(self):
        self.process = None
        self.functions = {}
        self.exported = set()
        self.patches = 0

    def start(self, exe):
        self.stop()
        # Read the symbols before the app starts: Buck may replace the file at `exe` later.
        self.functions = {
            name: address
            for name, (kind, address) in symbols(exe, "--defined-only").items()
            if kind in "tT"
        }
        self.exported = set(symbols(exe, "--dynamic", "--defined-only"))
        log("starting %s" % exe)
        # The app's stdout must not reach `buck2 dev`, which reads requests from ours.
        self.process = subprocess.Popen(
            [exe], stdin=subprocess.PIPE, stdout=sys.stderr, text=True
        )

    def stop(self):
        if self.process is not None and self.process.poll() is None:
            self.process.terminate()
            self.process.wait()
        self.process = None

    def patch(self, path):
        """Send the patch at `path` to the app. Returns False if the app needs a new binary."""
        new = symbols(path, "--defined-only")
        missing = [
            name
            for name in symbols(path, "--dynamic", "--undefined-only")
            if is_rust(name) and name not in self.exported
        ]
        if missing:
            # The new code calls a function that the running binary does not have, for
            # example a generic function with new type arguments.
            log("the patch uses %d functions the app does not have" % len(missing))
            return False

        table = {}
        for name, (kind, address) in new.items():
            if kind in "tT" and name in self.functions:
                table[str(self.functions[name])] = address

        self.patches += 1
        os.makedirs(PATCHES, exist_ok=True)
        lib = os.path.abspath(os.path.join(PATCHES, "patch-%d.so" % self.patches))
        shutil.copyfile(path, lib)
        message = {
            "lib": lib,
            "map": table,
            "aslr_reference": self.functions["main"],
            "new_base_address": new["main"][1],
            "ifunc_count": 0,
        }
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()
        log("sent patch %d with %d functions" % (self.patches, len(table)))
        return True


def main():
    # `buck2 dev` sends SIGTERM when it exits. Exit through the `finally` below.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    app = App()
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
