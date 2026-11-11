# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

"""A dev server for ES modules that `buck2 dev` builds as the browser asks for them.

It serves `/` from the `index` output. For `GET /some/path.js`, it asks `buck2 dev` for the output
`some/path.js`, which it defines with the handler `module` and `--path some/path.js` the first
time, waits for it, and serves the file. When `buck2 dev` says that an output is stale, the server
forgets the file it has, and tells the open pages to reload, on the event stream `/_events`. The
reload asks for the modules again, and only the stale ones are built.
"""

import http.server
import json
import os
import socket
import sys
import threading


class Outputs:
    """The outputs that the server has, and the builds that it waits for."""

    def __init__(self, request):
        self._request = request
        self._lock = threading.Condition()
        # Name to the path of the newest build, for the outputs that are not stale.
        self._built = {}
        # Name to the error of the last build, for outputs that failed.
        self._failed = {}
        # The outputs that pages wait for, and that no build has started for yet.
        self._wanted = set()
        # The outputs of the build that runs.
        self._building = None
        # The dynamic outputs that `buck2 dev` knows.
        self._defined = set()
        # The dynamic outputs that the next build defines.
        self._new = set()
        # The open event streams, one queue of events each.
        self._streams = []

    def get(self, name, dynamic):
        """The path of output `name`, built if need be, or raise `BuildError`."""
        with self._lock:
            while name not in self._built:
                if name in self._failed:
                    raise BuildError(self._failed[name])
                if name not in self._wanted and not self._is_building(name):
                    self._wanted.add(name)
                    if dynamic and name not in self._defined:
                        self._defined.add(name)
                        self._new.add(name)
                    self._start()
                self._lock.wait()
            return self._built[name]

    def _is_building(self, name):
        return self._building is not None and name in self._building

    def _start(self):
        """Send a build of the wanted outputs, unless one runs: they go into the next."""
        if self._building is not None or not self._wanted:
            return
        outputs = {}
        for name in sorted(self._wanted):
            if name in self._new:
                # The module's source file is its name.
                outputs[name] = {"handler": "module", "args": ["--path", name]}
            else:
                outputs[name] = None
        self._new = set()
        self._building = set(self._wanted)
        self._wanted = set()
        # Each module comes as soon as it is built.
        self._request({"type": "build", "outputs": outputs, "streaming": True})

    def on_message(self, message):
        kind = message.get("type")
        with self._lock:
            if kind == "built":
                for name, output in message["outputs"].items():
                    self._built[name] = output["path"]
                for name, error in message.get("failed", {}).items():
                    self._failed[name] = error
                    self._forget(name)
                if message["last_of_generation"]:
                    self._done()
            elif kind == "failed":
                for name in self._building or ():
                    self._failed[name] = message["error"]
                    self._forget(name)
                self._done()
            elif kind == "cancelled":
                # A file changed during the build: build what is still wanted again.
                for name in self._building or ():
                    if name not in self._built:
                        self._wanted.add(name)
                self._done()
            elif kind == "stale":
                for name in message["outputs"]:
                    self._built.pop(name, None)
                # Something changed: what failed may build now.
                self._failed.clear()
                sys.stderr.write("Stale: %s\n" % ", ".join(sorted(message["outputs"])))
                for stream in self._streams:
                    stream.append("reload")
            self._lock.notify_all()

    def _forget(self, name):
        """A module that failed is not tracked: what it would be built from is not known, so
        `buck2 dev` would say that it is stale after every change. A page that asks for it after
        the next `stale` defines it again."""
        if name in self._defined:
            self._defined.discard(name)
            self._request({"type": "forget", "outputs": [name]})

    def _done(self):
        self._building = None
        self._start()

    def events(self):
        """A new event stream: a list that `stale` adds `reload` to."""
        with self._lock:
            stream = []
            self._streams.append(stream)
            return stream

    def next_event(self, stream, timeout):
        with self._lock:
            if not stream:
                self._lock.wait(timeout)
            events = list(stream)
            stream.clear()
            return events

    def close(self, stream):
        with self._lock:
            self._streams.remove(stream)


class BuildError(Exception):
    pass


class Handler(http.server.BaseHTTPRequestHandler):
    outputs = None

    def do_GET(self):
        path = self.path.split("?", 1)[0]
        if path == "/_events":
            self.events()
            return
        if path in ("/", "/index.html"):
            name, dynamic, kind = "index", False, "text/html"
        elif path.endswith(".js") and ".." not in path.split("/"):
            name, dynamic, kind = path.lstrip("/"), True, "text/javascript"
        else:
            self.send_error(404)
            return
        try:
            built = self.outputs.get(name, dynamic)
        except BuildError as e:
            self.send_response(500)
            self.send_header("Content-Type", "text/plain")
            self.end_headers()
            self.wfile.write(str(e).encode())
            return
        with open(built, "rb") as f:
            body = f.read()
        self.send_response(200)
        self.send_header("Content-Type", kind)
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def events(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        stream = self.outputs.events()
        try:
            while True:
                events = self.outputs.next_event(stream, timeout=15)
                # A comment keeps the connection open, and finds out when it is closed.
                data = "".join("event: %s\ndata:\n\n" % e for e in events) or ":\n\n"
                self.wfile.write(data.encode())
                self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            self.outputs.close(stream)

    def log_message(self, format, *args):
        sys.stderr.write("%s\n" % (format % args))


def main():
    port = int(os.environ.get("PORT", "8000"))
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(os.environ["BUCK2_DEV_SOCKET"])
    conn = sock.makefile("rwb")
    send_lock = threading.Lock()

    def request(message):
        with send_lock:
            conn.write((json.dumps(message) + "\n").encode())
            conn.flush()

    outputs = Outputs(request)
    Handler.outputs = outputs
    server = http.server.ThreadingHTTPServer(("localhost", port), Handler)
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()

    request({"type": "hello", "version": 1})
    for line in conn:
        message = json.loads(line)
        if message.get("type") == "ready":
            sys.stderr.write("Serving on http://localhost:%d/\n" % port)
        elif message.get("type") == "shutdown":
            break
        outputs.on_message(message)
    server.shutdown()


if __name__ == "__main__":
    main()
