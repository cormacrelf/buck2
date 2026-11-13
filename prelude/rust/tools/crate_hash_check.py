#!/usr/bin/env python3
# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

"""
Checks that a `-Zno-codegen` hollow rlib and the rlib built with codegen
from the same sources record the same crate hash (SVH). Dependents compile
against the first and link against the second, so they must agree.

Usage:

    crate_hash_check.py --out=result.json \
        --pair <label> <hollow rlib> <codegen rlib> [--pair ...] [@argsfile]

Writes a Buck2 validation result to `--out`. The action itself succeeds
whatever the verdict, so that Buck2 reports the message.

`rustc -Zls=root` can print the crate hash, but it panics on the stub
metadata that `-Zembed-metadata=no` leaves in an rlib. This reads the
`CrateHeader` that starts every rmeta instead, which both kinds carry:

    b"rust\\0\\0\\0" METADATA_VERSION:u8 root_position:u64le
    rustc_version:str
    ... at root_position:
    CrateHeader { triple: TargetTuple, hash: Svh, name: Symbol, ... }

where `str` is a LEB128 length, the bytes, then the sentinel 0xC1, and
`TargetTuple` is an enum tag byte followed by its strings.
"""

import argparse
import json
import struct
import sys
from typing import List, Tuple

METADATA_MAGIC = b"rust\0\0\0"
KNOWN_METADATA_VERSIONS = (10,)
STR_SENTINEL = 0xC1
AR_MAGIC = b"!<arch>\n"
AR_HEADER_LEN = 60
RMETA_MEMBER = "lib.rmeta"


class DecodeError(Exception):
    pass


def ar_member(data: bytes, wanted: str) -> bytes:
    """The contents of the member called `wanted` of a GNU or BSD archive."""
    if not data.startswith(AR_MAGIC):
        raise DecodeError("not an ar archive")
    pos = len(AR_MAGIC)
    long_names = b""
    while pos + AR_HEADER_LEN <= len(data):
        header = data[pos : pos + AR_HEADER_LEN]
        raw_name = header[0:16].decode("ascii", "replace").rstrip(" ")
        size = int(header[48:58].decode("ascii").strip())
        body = data[pos + AR_HEADER_LEN : pos + AR_HEADER_LEN + size]
        pos += AR_HEADER_LEN + size + (size & 1)

        if raw_name.startswith("#1/"):
            # BSD: the name precedes the contents, inside `size`.
            name_len = int(raw_name[3:])
            name = body[:name_len].rstrip(b"\0").decode("utf-8", "replace")
            body = body[name_len:]
        elif raw_name == "//":
            long_names = body
            continue
        elif raw_name in ("/", "/SYM64/", "__.SYMDEF", "__.SYMDEF SORTED"):
            continue
        elif raw_name.startswith("/") and raw_name[1:].isdigit():
            offset = int(raw_name[1:])
            end = long_names.find(b"/\n", offset)
            name = long_names[offset:end].decode("utf-8", "replace")
        else:
            name = raw_name.rstrip("/")

        if name == wanted:
            return body
    raise DecodeError("no `{}` member".format(wanted))


def _leb128(data: bytes, pos: int) -> Tuple[int, int]:
    value = 0
    shift = 0
    while True:
        if pos >= len(data):
            raise DecodeError("truncated LEB128")
        byte = data[pos]
        pos += 1
        value |= (byte & 0x7F) << shift
        if byte & 0x80 == 0:
            return value, pos
        shift += 7


def _skip_str(data: bytes, pos: int) -> int:
    length, pos = _leb128(data, pos)
    pos += length
    if pos >= len(data) or data[pos] != STR_SENTINEL:
        raise DecodeError("malformed string at offset {}".format(pos))
    return pos + 1


def crate_hash(rmeta: bytes) -> str:
    """The crate hash of an rmeta blob, possibly wrapped in an object file,
    as the hex that `rustc -Zls=root` prints."""
    start = rmeta.find(METADATA_MAGIC)
    if start < 0:
        raise DecodeError("no rustc metadata header")
    blob = rmeta[start:]
    if len(blob) < 16:
        raise DecodeError("truncated metadata header")
    version = blob[len(METADATA_MAGIC)]
    if version not in KNOWN_METADATA_VERSIONS:
        raise DecodeError(
            "metadata version {} is not one this tool decodes ({})".format(
                version, ", ".join(str(v) for v in KNOWN_METADATA_VERSIONS)
            )
        )
    (root,) = struct.unpack_from("<Q", blob, len(METADATA_MAGIC) + 1)
    pos = root

    # CrateHeader.triple
    if pos >= len(blob):
        raise DecodeError("root position {} is out of bounds".format(root))
    tag = blob[pos]
    pos += 1
    if tag == 0:
        # TargetTuple::TargetTuple(String)
        pos = _skip_str(blob, pos)
    elif tag == 1:
        # TargetTuple::TargetJson { tuple, contents }
        pos = _skip_str(blob, pos)
        pos = _skip_str(blob, pos)
    else:
        raise DecodeError("unknown TargetTuple tag {}".format(tag))

    # CrateHeader.hash: a Fingerprint, encoded as 16 little-endian bytes.
    svh = blob[pos : pos + 16]
    if len(svh) != 16:
        raise DecodeError("truncated crate hash")
    return svh[::-1].hex()


def rlib_crate_hash(path: str) -> str:
    with open(path, "rb") as f:
        data = f.read()
    return crate_hash(ar_member(data, RMETA_MEMBER))


def check(pairs: List[Tuple[str, str, str]]) -> Tuple[bool, str]:
    errors = []
    mismatched = False
    for label, hollow, codegen in pairs:
        try:
            hollow_hash = rlib_crate_hash(hollow)
            codegen_hash = rlib_crate_hash(codegen)
        except (DecodeError, OSError, ValueError) as e:
            errors.append("{}: could not read crate hash: {}".format(label, e))
            continue
        if hollow_hash != codegen_hash:
            mismatched = True
            errors.append(
                "{}: crate hash of the hollow rlib differs from the codegen rlib\n"
                "  {}  {}\n"
                "  {}  {}".format(label, hollow_hash, hollow, codegen_hash, codegen)
            )
    if mismatched:
        errors.append(
            "This is usually due to nondeterminism in proc macros or generated code."
        )
    if errors:
        return False, "\n".join(errors)
    return True, "{} crates have matching hashes".format(len(pairs))


def main() -> None:
    parser = argparse.ArgumentParser(fromfile_prefix_chars="@")
    parser.add_argument("--out", required=True)
    parser.add_argument(
        "--pair",
        nargs=3,
        action="append",
        default=[],
        metavar=("LABEL", "HOLLOW_RLIB", "CODEGEN_RLIB"),
    )
    args = parser.parse_args()

    ok, message = check([tuple(p) for p in args.pair])
    with open(args.out, "w") as f:
        json.dump(
            {
                "version": 1,
                "data": {
                    "status": "success" if ok else "failure",
                    "message": message,
                },
            },
            f,
        )
    if not ok:
        print(message, file=sys.stderr)


if __name__ == "__main__":
    main()
