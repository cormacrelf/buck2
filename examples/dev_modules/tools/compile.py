# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

"""Compile an ES module: `compile.py PATH SRC OUT`. This stands in for a real compiler, such as
one for TypeScript: it adds a comment that names PATH, the module's source file in the project,
and leaves the rest as it is."""

import sys


def main():
    path, src, out = sys.argv[1:]
    with open(src) as f:
        code = f.read()
    with open(out, "w") as f:
        f.write("// Compiled from %s\n" % path)
        f.write(code)


if __name__ == "__main__":
    main()
