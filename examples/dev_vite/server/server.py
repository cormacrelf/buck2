# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

"""Runs the Vite dev server on the files that `buck2 dev` builds.

`buck2 dev` builds two outputs:

- `packages`: the installed `node_modules`.
- `app`: the app's files, sources and generated files together.

Buck replaces an output as a whole when it builds it again, and Vite's file watcher does
not follow a directory that is replaced. So this server copies each new `app` into a
directory that stays in place, `buck-out/dev_vite/app`, and writes only the files whose
content changed. Vite sees those writes and updates the browser with hot module
replacement. When `packages` changes, the server starts Vite again.
"""

import argparse
import ctypes
import json
import os
import shutil
import signal
import subprocess
import sys

STAGE = os.path.join("buck-out", "dev_vite", "app")


def log(message):
    sys.stderr.write("[dev_vite] %s\n" % message)
    sys.stderr.flush()


def request(message):
    """Send a request to `buck2 dev` on stdout."""
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def read(path):
    with open(path, "rb") as f:
        return f.read()


def sync(src, dst):
    """Make `dst` hold the files of `src`, writing only files whose content changed.

    `node_modules` and Vite's cache, `.vite`, in `dst` are left alone. Returns the paths
    written or removed.
    """
    changes = []
    wanted = set()
    for root, _dirs, files in os.walk(src, followlinks=True):
        rel_root = os.path.relpath(root, src)
        for name in files:
            rel = os.path.normpath(os.path.join(rel_root, name))
            wanted.add(rel)
            new = read(os.path.join(root, name))
            target = os.path.join(dst, rel)
            if os.path.isfile(target) and read(target) == new:
                continue
            os.makedirs(os.path.dirname(target), exist_ok=True)
            # Write in place so that Vite sees a change to the file, not a new file.
            with open(target, "wb") as f:
                f.write(new)
            changes.append(rel)
    for root, dirs, files in os.walk(dst):
        if root == dst:
            dirs[:] = [d for d in dirs if d not in ("node_modules", ".vite")]
        for name in files:
            path = os.path.join(root, name)
            if os.path.relpath(path, dst) not in wanted:
                os.remove(path)
                changes.append("-" + os.path.relpath(path, dst))
    return changes


def die_with_parent():
    """In the Vite process, before it starts: on Linux, get SIGTERM when this server dies,
    even if the server is killed with SIGKILL."""
    if sys.platform.startswith("linux"):
        PR_SET_PDEATHSIG = 1
        ctypes.CDLL(None).prctl(PR_SET_PDEATHSIG, signal.SIGTERM)


class Vite:
    def __init__(self, port):
        self.port = port
        self.process = None

    def start(self, packages):
        self.stop()
        link = os.path.join(STAGE, "node_modules")
        if os.path.islink(link) or os.path.exists(link):
            os.remove(link)
        os.symlink(os.path.join(packages, "node_modules"), link)
        vite = os.path.join(packages, "node_modules", "vite", "bin", "vite.js")
        log("starting Vite on http://localhost:%d/" % self.port)
        # Vite must not read our stdin, which carries `buck2 dev` messages; it reads stdin for
        # its keyboard shortcuts. Its stdout must not reach `buck2 dev`, which reads requests
        # from ours.
        self.process = subprocess.Popen(
            ["node", vite, "--port", str(self.port), "--strictPort"],
            cwd=STAGE,
            stdin=subprocess.DEVNULL,
            stdout=sys.stderr,
            preexec_fn=die_with_parent,
        )

    def stop(self):
        if self.process is not None and self.process.poll() is None:
            self.process.terminate()
            self.process.wait()
        self.process = None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=5173)
    args = parser.parse_args()

    # `buck2 dev` sends SIGTERM when it exits. Exit through the `finally` below, which stops Vite.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    vite = Vite(int(os.environ.get("PORT", args.port)))
    packages = None
    have_app = False
    try:
        for line in sys.stdin:
            message = json.loads(line)
            kind = message["type"]
            if kind == "ready":
                request({"type": "build", "outputs": ["packages", "app"]})
            elif kind == "built":
                outputs = message["outputs"]
                if "app" in outputs:
                    os.makedirs(STAGE, exist_ok=True)
                    changes = sync(outputs["app"]["path"], STAGE)
                    have_app = True
                    if changes:
                        log("updated " + " ".join(changes))
                if "packages" in outputs and outputs["packages"]["path"] != packages:
                    packages = outputs["packages"]["path"]
                    vite.start(packages)
                elif "packages" in outputs and outputs["packages"]["changed"]:
                    vite.start(packages)
                if have_app and packages is not None and vite.process is None:
                    vite.start(packages)
            elif kind == "failed":
                log("build failed; Vite keeps serving the last build")
    finally:
        # `buck2 dev` closed stdin: it has exited.
        vite.stop()


if __name__ == "__main__":
    main()
