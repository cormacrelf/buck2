# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

"""Writes a JavaScript module that exports the list of strings in a JSON file."""

import json
import sys


def main():
    src, out = sys.argv[1:]
    with open(src) as f:
        messages = json.load(f)
    with open(out, "w") as f:
        f.write("export const messages = %s;\n" % json.dumps(messages, indent=2))


if __name__ == "__main__":
    main()
