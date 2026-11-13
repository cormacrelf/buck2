#!/usr/bin/env fbpython
# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

import pathlib
import shutil
import struct
import tempfile
import unittest

from crate_hash_check import ar_member, check, crate_hash, DecodeError

HASH = bytes(range(16))
HASH_HEX = HASH[::-1].hex()


def rstr(s: bytes) -> bytes:
    assert len(s) < 0x80
    return bytes([len(s)]) + s + b"\xc1"


def rmeta(svh: bytes = HASH, version: int = 10, tuple_tag: int = 0) -> bytes:
    """An rmeta wrapped in some object-file noise, as rustc lays out its header."""
    rustc_version = rstr(b"rustc 1.99.0-nightly")
    root = 8 + 8 + len(rustc_version)
    triple = bytes([tuple_tag]) + rstr(b"x86_64-unknown-linux-gnu")
    if tuple_tag == 1:
        triple += rstr(b'{"json": true}')
    blob = (
        b"rust\0\0\0"
        + bytes([version])
        + struct.pack("<Q", root)
        + rustc_version
        + triple
        + svh
        + rstr(b"foo")
    )
    return b"\x7fELF" + b"\0" * 60 + blob + b"\0" * 32


def ar_entry(name: bytes, body: bytes) -> bytes:
    header = name.ljust(16) + b"0".ljust(12) + b"0".ljust(6) + b"0".ljust(6)
    header += b"644".ljust(8) + str(len(body)).encode().ljust(10) + b"`\n"
    return header + body + (b"\n" if len(body) & 1 else b"")


def gnu_rlib(meta: bytes) -> bytes:
    return (
        b"!<arch>\n"
        + ar_entry(b"/", b"\0\0\0\0")
        + ar_entry(b"lib.rmeta/", meta)
        + ar_entry(b"foo.cgu.0.rcgu.o/", b"code")
    )


def bsd_rlib(meta: bytes) -> bytes:
    name = b"lib.rmeta\0\0\0"
    return (
        b"!<arch>\n"
        + ar_entry(b"#1/" + str(len(name)).encode(), name + meta)
        + ar_entry(b"#1/4", b"objsdata")
    )


class CrateHashTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = pathlib.Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp)

    def write(self, name: str, data: bytes) -> str:
        path = self.tmp / name
        path.write_bytes(data)
        return str(path)

    def test_hash_after_target_tuple(self) -> None:
        self.assertEqual(crate_hash(rmeta()), HASH_HEX)

    def test_hash_after_target_json(self) -> None:
        self.assertEqual(crate_hash(rmeta(tuple_tag=1)), HASH_HEX)

    def test_unknown_version_is_an_error(self) -> None:
        with self.assertRaises(DecodeError):
            crate_hash(rmeta(version=11))

    def test_gnu_and_bsd_archives(self) -> None:
        self.assertEqual(ar_member(gnu_rlib(rmeta()), "lib.rmeta"), rmeta())
        self.assertEqual(ar_member(bsd_rlib(rmeta()), "lib.rmeta"), rmeta())

    def test_check(self) -> None:
        hollow = self.write("hollow.rlib", gnu_rlib(rmeta()))
        same = self.write("same.rlib", bsd_rlib(rmeta()))
        other = self.write("other.rlib", gnu_rlib(rmeta(svh=bytes(16))))
        self.assertEqual(
            check([("a", hollow, same)]), (True, "1 crates have matching hashes")
        )
        ok, message = check([("a", hollow, same), ("b", hollow, other)])
        self.assertFalse(ok)
        self.assertNotIn("a:", message)
        self.assertIn("b: crate hash of the hollow rlib differs", message)
        self.assertIn("nondeterminism in proc macros or generated code", message)
        ok, message = check([("c", hollow, self.write("bad.rlib", b"junk"))])
        self.assertFalse(ok)
        self.assertIn("not an ar archive", message)
        self.assertNotIn("nondeterminism", message)


if __name__ == "__main__":
    unittest.main()
