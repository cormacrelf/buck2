# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# A `buck2 dev` server for the tests. It appends each message it receives, with its process ID
# and the content of each built output, to `dev_events.jsonl` in the project root.
#
# On `ready` it asks for the `site` output. With `--build-on-stale`, it asks for `site` again
# whenever `buck2 dev` reports files of a `notify` set as changed. With `--bad-request`, it also
# sends a request of an unknown type on `ready`.

import json
import os
import sys


def log(message):
    message["pid"] = os.getpid()
    if message["type"] == "built":
        for output in message["outputs"].values():
            with open(output["path"]) as f:
                output["content"] = f.read()
    with open("dev_events.jsonl", "a") as f:
        f.write(json.dumps(message) + "\n")


def request(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def main():
    build_on_stale = "--build-on-stale" in sys.argv[1:]
    bad_request = "--bad-request" in sys.argv[1:]
    # Lines that are not requests are printed by `buck2 dev`.
    print("server started", flush=True)
    for line in sys.stdin:
        message = json.loads(line)
        log(message)
        if message["type"] == "ready":
            if bad_request:
                request({"type": "bulid", "outputs": ["site"]})
            request({"type": "build", "outputs": ["site"]})
        elif message["type"] == "stale" and message["files"] and build_on_stale:
            request({"type": "build", "outputs": ["site"]})
    # `buck2 dev` closed stdin: it has exited.


if __name__ == "__main__":
    main()
