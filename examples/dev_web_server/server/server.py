# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

"""A web server for a directory that `buck2 dev` rebuilds.

Under `buck2 dev`, the server connects to the socket in `BUCK2_DEV_SOCKET`, and exchanges
JSON lines with `buck2 dev` there. It asks for the `www` output when it starts, and serves the
newest `www` directory it has been told about. A browser gets the new pages on its next request.

With `--root DIR` (`buck2 run`), it serves DIR and does not connect to `buck2 dev`.
"""

import argparse
import functools
import http.server
import json
import os
import socket
import sys
import threading


class Site:
    """The directory being served. `buck2 dev` replaces it after each build."""

    def __init__(self, root):
        self._lock = threading.Lock()
        self._root = root

    def get(self):
        with self._lock:
            return self._root

    def set(self, root):
        with self._lock:
            self._root = root


class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, site, **kwargs):
        self.site = site
        super().__init__(*args, **kwargs)

    def translate_path(self, path):
        # Resolve each request against the current build, not the one at start-up.
        root = self.site.get()
        rel = os.path.relpath(super().translate_path(path), os.getcwd())
        return os.path.join(root, rel)

    def do_GET(self):
        if self.site.get() is None:
            self.send_error(503, "The site has not been built yet")
            return
        super().do_GET()


def connect_to_buck2_dev():
    """Connect to `buck2 dev`. Returns a file to read and write JSON lines on."""
    address = os.environ["BUCK2_DEV_SOCKET"]
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(address)
    return sock.makefile("rwb")


def follow_buck2_dev(site, server):
    conn = connect_to_buck2_dev()

    def request(message):
        conn.write((json.dumps(message) + "\n").encode())
        conn.flush()

    request({"type": "hello", "version": 1})
    for line in conn:
        message = json.loads(line)
        # Messages of other types may be added; ignore them.
        kind = message.get("type")
        if kind == "ready":
            request({"type": "build", "outputs": ["www"]})
        elif kind == "building":
            sys.stderr.write("Building the site...\n")
        elif kind == "built":
            www = message["outputs"].get("www")
            if www is not None:
                site.set(www["path"])
                sys.stderr.write("Serving the new site (build %d)\n" % message["gen"])
        elif kind == "failed":
            sys.stderr.write("Build failed; still serving the last site\n")
        elif kind == "shutdown":
            break
    # `buck2 dev` is stopping this server, or has exited.
    server.shutdown()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=8000)
    parser.add_argument("--root", help="Serve this directory instead of following buck2 dev")
    args = parser.parse_args()
    port = int(os.environ.get("PORT", args.port))

    site = Site(os.path.abspath(args.root) if args.root else None)
    handler = functools.partial(Handler, site=site)
    server = http.server.ThreadingHTTPServer(("localhost", port), handler)
    if args.root is None:
        threading.Thread(
            target=follow_buck2_dev, args=(site, server), daemon=True
        ).start()
    sys.stderr.write("Serving on http://localhost:%d/\n" % port)
    server.serve_forever()
    server.server_close()


if __name__ == "__main__":
    main()
