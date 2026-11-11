# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

"""Runs the Vite dev server on the files that `buck2 dev` builds.

`buck2 dev` builds two outputs, when this server asks for them: when it starts, and when
`buck2 dev` says that they are stale:

- `packages`: the installed `node_modules`.
- `app`: the app's files, sources and generated files together.

Buck replaces an output as a whole when it builds it again, and Vite's file watcher does
not follow a directory that is replaced. So this server keeps the app's files in a
directory that stays in place, `buck-out/dev_vite/app`. Each file of the app is a sidecar
of `app`, and `built` says where each one is and whether it changed: the server writes the
changed ones, and reads and compares nothing. Vite sees those writes and updates the
browser with hot module replacement. When `packages` changes, the server starts Vite again.

Vite runs in this server's process group, so `buck2 dev` stops it with the server. The
arguments after `--` go to Vite: `buck2 dev //app:app -- --host` passes `--host`.
"""

import argparse
import json
import os
import shutil
import signal
import socket
import subprocess
import sys

STAGE = os.path.join("buck-out", "dev_vite", "app")


def log(message):
    sys.stderr.write("[dev_vite] %s\n" % message)
    sys.stderr.flush()


def connect_to_buck2_dev():
    """Connect to `buck2 dev`. Returns a file to read and write JSON lines on."""
    address = os.environ["BUCK2_DEV_SOCKET"]
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(address)
    return sock.makefile("rwb")


def sync(files, dst):
    """Make `dst` hold `files`: the sidecars of `app`, by their paths in the app.

    `buck2 dev` says of each whether it changed since the last build that this server got,
    so only those are written. `node_modules` and Vite's cache, `.vite`, in `dst` are left
    alone. Returns the paths written or removed.
    """
    changes = []
    for rel, file in files.items():
        target = os.path.join(dst, rel)
        if not file["changed"] and os.path.isfile(target):
            continue
        os.makedirs(os.path.dirname(target), exist_ok=True)
        # Write in place so that Vite sees a change to the file, not a new file.
        with open(file["path"], "rb") as new, open(target, "wb") as f:
            shutil.copyfileobj(new, f)
        changes.append(rel)
    # A file that the app no longer has.
    for root, dirs, names in os.walk(dst):
        if root == dst:
            dirs[:] = [d for d in dirs if d not in ("node_modules", ".vite")]
        for name in names:
            path = os.path.join(root, name)
            if os.path.relpath(path, dst) not in files:
                os.remove(path)
                changes.append("-" + os.path.relpath(path, dst))
    return changes


class Vite:
    def __init__(self, port, args):
        self.port = port
        # Arguments for Vite, from the command line.
        self.args = args
        self.process = None

    def start(self, packages):
        self.stop()
        link = os.path.join(STAGE, "node_modules")
        if os.path.islink(link) or os.path.exists(link):
            os.remove(link)
        os.symlink(os.path.join(packages, "node_modules"), link)
        vite = os.path.join(packages, "node_modules", "vite", "bin", "vite.js")
        argv = ["node", vite]
        # Vite takes an option given twice as a list, so a `--port` of the user's replaces this
        # server's.
        if any(arg == "--port" or arg.startswith("--port=") for arg in self.args):
            log("starting Vite")
        else:
            log("starting Vite on http://localhost:%d/" % self.port)
            argv += ["--port", str(self.port), "--strictPort"]
        # Vite inherits the terminal, which `DevInfo(interactive = True)` gives this server, so
        # its keyboard shortcuts work.
        self.process = subprocess.Popen(argv + self.args, cwd=STAGE)

    def stop(self):
        if self.process is not None and self.process.poll() is None:
            self.process.terminate()
            self.process.wait()
        self.process = None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=5173)
    parser.add_argument("vite_args", nargs="*", help="arguments for Vite, after `--`")
    args = parser.parse_args()

    # `buck2 dev` may stop this server with SIGTERM. Exit through the `finally` below, which
    # stops Vite.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    vite = Vite(int(os.environ.get("PORT", args.port)), args.vite_args)
    packages = None
    have_app = False
    conn = connect_to_buck2_dev()

    def request(message):
        conn.write((json.dumps(message) + "\n").encode())
        conn.flush()

    try:
        request({"type": "hello", "version": 1})
        for line in conn:
            message = json.loads(line)
            # Messages of other types may be added; ignore them.
            kind = message.get("type")
            if kind == "ready":
                request({"type": "build", "outputs": ["packages", "app"]})
            elif kind == "stale":
                # A file that the app or the packages are built from changed.
                request({"type": "build", "outputs": list(message["outputs"])})
            elif kind == "built":
                outputs = message["outputs"]
                if "app" in outputs:
                    os.makedirs(STAGE, exist_ok=True)
                    changes = sync(outputs["app"]["sidecars"], STAGE)
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
            elif kind == "shutdown":
                break
    finally:
        # `buck2 dev` is stopping this server, or has exited.
        vite.stop()


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        # Ctrl-C, which reaches this server because it is interactive. `main` has stopped Vite.
        # Die of SIGINT, without a traceback, so that `buck2 dev` sees the user quit.
        signal.signal(signal.SIGINT, signal.SIG_DFL)
        os.kill(os.getpid(), signal.SIGINT)
