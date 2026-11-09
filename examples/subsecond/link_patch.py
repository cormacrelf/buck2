# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

"""Links the object files of an rlib into a shared library that subsecond can load.

Usage: link_patch.py CC RLIB OUT

The library leaves the symbols of other crates undefined. When the app loads it, the dynamic
loader finds them in the app's binary, which is linked with `--export-dynamic`.
"""

import os
import subprocess
import sys
import tempfile


def objects(rlib, out_dir):
    """Write the object files of the `ar` archive `rlib` to `out_dir`, and return their paths."""
    paths = []
    with open(rlib, "rb") as f:
        if f.read(8) != b"!<arch>\n":
            raise SystemExit("%s is not an ar archive" % rlib)
        names = b""
        while True:
            header = f.read(60)
            if len(header) < 60:
                break
            name = header[:16].decode().strip()
            size = int(header[48:58])
            data = f.read(size)
            if size % 2:
                f.read(1)
            if name == "//":
                names = data
                continue
            if name.startswith("/") and name[1:].isdigit():
                # A GNU long name: an offset into the `//` member.
                start = int(name[1:])
                name = names[start : names.index(b"/\n", start)].decode()
            name = name.rstrip("/")
            if name.endswith(".o"):
                path = os.path.join(out_dir, name)
                with open(path, "wb") as o:
                    o.write(data)
                paths.append(path)
    return paths


def main():
    cc, rlib, out = sys.argv[1:]
    with tempfile.TemporaryDirectory(dir=os.environ.get("BUCK_SCRATCH_PATH")) as tmp:
        # `subsecond::apply_patch` finds where the library was loaded from the address of
        # its `main`, and the server reads that address from the library. This `main` is
        # never called.
        main = os.path.join(tmp, "main.c")
        with open(main, "w") as f:
            f.write("int main(void) { return 0; }\n")
        subprocess.run(
            [
                cc,
                "-shared",
                "-o",
                out,
                "-fPIC",
                main,
                *objects(rlib, tmp),
                # Calls between functions of the patch go to the patch, not to the old
                # copies in the binary.
                "-Wl,-Bsymbolic",
            ],
            check=True,
        )


if __name__ == "__main__":
    main()
