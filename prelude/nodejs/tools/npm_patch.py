# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

"""Apply the patch files that `pnpm patch-commit` writes to an extracted npm package.

The patch files use git's diff format. apply_patch reads one, optionally
checks it against the hash the lockfile recorded, and applies it to a package
directory. Files are handled as bytes and every line keeps its own ending, so
files with CRLF line endings patch correctly when the patch carries CRLF lines.

Context and removed lines must match exactly. Each hunk is tried at the
position its header states, shifted by however far the earlier hunks of the
same file had to move. If it does not match there, the nearest position where
it matches exactly is used, as GNU patch does without fuzz. Hunks of one file
are applied in order and may not overlap.

Every failure is a ValueError whose message names the patch file, the file
inside the package and, for a hunk, the hunk header.

Supported: modified files, new files (modes 100644 and 100755), deleted files
(with or without hunks), mode changes, renames (with or without hunks), the
"No newline at end of file" marker, git's quoted paths, hunk headers without
counts, and patch files whose sections begin at `---` without a `diff --git`
line. Binary patches, copies, symlinks and submodules are rejected.
"""

from __future__ import annotations

import hashlib
import os
import re
from dataclasses import dataclass, field
from pathlib import Path

_DEV_NULL = b"/dev/null"
_HUNK_RE = re.compile(rb"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@(.*)$")
_SHA256_RE = re.compile(r"[0-9a-fA-F]{64}")
_FILE_MODES = {b"100644": 0o644, b"100755": 0o755}
_SIMPLE_ESCAPES = {
    ord("a"): 7,
    ord("b"): 8,
    ord("f"): 12,
    ord("n"): 10,
    ord("r"): 13,
    ord("t"): 9,
    ord("v"): 11,
    ord("\\"): ord("\\"),
    ord('"'): ord('"'),
}


@dataclass
class _Hunk:
    header: str
    old_start: int
    old_count: int
    old: list[bytes] = field(default_factory=list)
    new: list[bytes] = field(default_factory=list)


@dataclass
class _FilePatch:
    # The `diff --git` line, or the `---` line of a section without one.
    header: str
    git_old: bytes | None = None
    git_new: bytes | None = None
    minus: bytes | None = None
    plus: bytes | None = None
    old_mode: bytes | None = None
    new_mode: bytes | None = None
    new_file_mode: bytes | None = None
    deleted_file_mode: bytes | None = None
    rename_from: bytes | None = None
    rename_to: bytes | None = None
    saw_minus: bool = False
    hunks: list[_Hunk] = field(default_factory=list)

    def known_name(self) -> str | None:
        for name in (self.rename_to, self.minus, self.plus, self.git_new):
            if name is not None and name != _DEV_NULL:
                return _text(name)
        return None


def _fail(patch: Path, file: str | None, message: str, hunk: str | None = None) -> ValueError:
    where = f"{patch}"
    if file is not None:
        where += f": {file}"
    if hunk is not None:
        where += f": hunk {hunk}"
    return ValueError(f"{where}: {message}")


def _split_lines(data: bytes) -> list[bytes]:
    """Split on b"\\n" only, keeping each line's ending."""
    parts = data.split(b"\n")
    lines = [part + b"\n" for part in parts[:-1]]
    if parts[-1]:
        lines.append(parts[-1])
    return lines


def _text(name: bytes) -> str:
    return os.fsdecode(name)


def _parse_quoted(data: bytes) -> tuple[bytes, bytes]:
    """Parse a git-quoted name at the start of data, returning it and the remainder."""
    out = bytearray()
    i = 1
    while i < len(data):
        c = data[i]
        if c == ord('"'):
            return bytes(out), data[i + 1 :]
        if c != ord("\\"):
            out.append(c)
            i += 1
            continue
        i += 1
        if i >= len(data):
            break
        c = data[i]
        if c in _SIMPLE_ESCAPES:
            out.append(_SIMPLE_ESCAPES[c])
            i += 1
        elif ord("0") <= c <= ord("7"):
            j = i
            while j < len(data) and j < i + 3 and ord("0") <= data[j] <= ord("7"):
                j += 1
            value = int(data[i:j], 8)
            if value > 255:
                raise ValueError("octal escape out of range")
            out.append(value)
            i = j
        else:
            raise ValueError("unknown escape in quoted name")
    raise ValueError("unterminated quoted name")


def _parse_name(data: bytes) -> bytes:
    """Parse the name on a `---`, `+++` or rename line."""
    data = data.rstrip(b"\r\n")
    if data.startswith(b'"'):
        name, rest = _parse_quoted(data)
        if rest.strip():
            raise ValueError("junk after quoted name")
        return name
    # Git and diff -u put a tab after names that contain spaces, and
    # diff -u follows it with a timestamp.
    return data.split(b"\t", 1)[0]


def _parse_git_line(rest: bytes) -> tuple[bytes, bytes]:
    """Split the arguments of `diff --git` into the old and new name."""
    rest = rest.rstrip(b"\r\n")
    if rest.startswith(b'"'):
        old, remainder = _parse_quoted(rest)
        remainder = remainder.lstrip(b" ")
        if not remainder.startswith(b'"'):
            return old, remainder
        new, remainder = _parse_quoted(remainder)
        if remainder:
            raise ValueError("junk after quoted name")
        return old, new
    quoted = rest.find(b' "')
    if quoted >= 0:
        new, remainder = _parse_quoted(rest[quoted + 1 :])
        if remainder:
            raise ValueError("junk after quoted name")
        return rest[:quoted], new
    # Unquoted names may contain spaces. Without a rename both names are the
    # same, so prefer the split where they are.
    splits = [i for i in range(len(rest)) if rest.startswith(b" b/", i)]
    for i in splits:
        if rest[2:i] == rest[i + 3 :]:
            return rest[:i], rest[i + 1 :]
    if splits:
        return rest[: splits[0]], rest[splits[0] + 1 :]
    raise ValueError("cannot split names")


def _parse_patch(patch: Path, data: bytes) -> list[_FilePatch]:
    lines = _split_lines(data)
    sections: list[_FilePatch] = []
    cur: _FilePatch | None = None
    i = 0
    while i < len(lines):
        stripped = lines[i].rstrip(b"\r\n")
        i += 1

        if stripped.startswith(b"diff --git "):
            cur = _FilePatch(header=_text(stripped))
            sections.append(cur)
            try:
                cur.git_old, cur.git_new = _parse_git_line(stripped[len(b"diff --git ") :])
            except ValueError:
                pass  # Only a problem if no other line names the file.
            continue

        if stripped.startswith(b"--- ") and (cur is None or cur.saw_minus or cur.hunks):
            cur = _FilePatch(header=_text(stripped))
            sections.append(cur)
        if cur is None:
            continue  # Preamble.
        if stripped.startswith(b"--- ") and not cur.saw_minus:
            if i >= len(lines) or not lines[i].startswith(b"+++ "):
                raise _fail(patch, cur.known_name(), f"`---` line is not followed by `+++`: {_text(stripped)}")
            try:
                cur.minus = _parse_name(stripped[4:])
                cur.plus = _parse_name(lines[i][4:])
            except ValueError as e:
                raise _fail(patch, None, f"bad file name in {_text(stripped)}: {e}")
            cur.saw_minus = True
            i += 1
            continue

        if stripped.startswith(b"@@"):
            m = _HUNK_RE.match(stripped)
            if m is None:
                raise _fail(patch, cur.known_name(), f"malformed hunk header: {_text(stripped)}")
            if not cur.saw_minus:
                raise _fail(patch, cur.known_name(), f"hunk without `---`/`+++` lines: {_text(stripped)}")
            hunk = _Hunk(
                header=_text(stripped),
                old_start=int(m.group(1)),
                old_count=1 if m.group(2) is None else int(m.group(2)),
            )
            new_count = 1 if m.group(4) is None else int(m.group(4))
            i = _parse_hunk_body(patch, cur.known_name(), hunk, new_count, lines, i)
            cur.hunks.append(hunk)
            continue

        if not stripped:
            continue
        if cur.hunks:
            raise _fail(patch, cur.known_name(), f"unexpected line after hunk: {_text(stripped)}")
        if stripped.startswith((b"GIT binary patch", b"Binary files ")):
            raise _fail(patch, cur.known_name() or _text(cur.git_new or b""), "binary patches are not supported")
        if stripped.startswith((b"copy from ", b"copy to ")):
            raise _fail(patch, cur.known_name() or _text(cur.git_new or b""), "copies are not supported")
        for prefix, attr in (
            (b"old mode ", "old_mode"),
            (b"new mode ", "new_mode"),
            (b"new file mode ", "new_file_mode"),
            (b"deleted file mode ", "deleted_file_mode"),
        ):
            if stripped.startswith(prefix):
                setattr(cur, attr, stripped[len(prefix) :].strip())
                break
        else:
            for prefix, attr in ((b"rename from ", "rename_from"), (b"rename to ", "rename_to")):
                if stripped.startswith(prefix):
                    try:
                        setattr(cur, attr, _parse_name(stripped[len(prefix) :]))
                    except ValueError as e:
                        raise _fail(patch, cur.known_name(), f"bad file name in {_text(stripped)}: {e}")
                    break
            else:
                if not stripped.startswith((b"index ", b"similarity index ", b"dissimilarity index ")):
                    raise _fail(patch, cur.known_name(), f"unrecognised line: {_text(stripped)}")
    return sections


def _chomp(line: bytes) -> bytes:
    return line[:-1] if line.endswith(b"\n") else line


def _parse_hunk_body(
    patch: Path,
    file: str | None,
    hunk: _Hunk,
    new_count: int,
    lines: list[bytes],
    i: int,
) -> int:
    """Read a hunk's lines starting at lines[i], returning the index after them."""
    old_seen = new_seen = 0
    last: tuple[bool, bool] | None = None  # Which sides the previous line is on.
    while True:
        if i < len(lines) and lines[i].startswith(b"\\"):
            if last is None:
                raise _fail(patch, file, "`\\ No newline` marker without a line before it", hunk.header)
            if last[0]:
                hunk.old[-1] = _chomp(hunk.old[-1])
            if last[1]:
                hunk.new[-1] = _chomp(hunk.new[-1])
            i += 1
            last = None
            continue
        if old_seen >= hunk.old_count and new_seen >= new_count:
            return i
        if i >= len(lines):
            raise _fail(patch, file, "hunk is truncated", hunk.header)
        line = lines[i]
        i += 1
        tag = line[:1]
        if tag == b" " or line in (b"\n", b"\r\n"):
            # A bare empty line is a context line whose space was stripped.
            body = line[1:] if tag == b" " else line
            hunk.old.append(body)
            hunk.new.append(body)
            old_seen += 1
            new_seen += 1
            last = (True, True)
        elif tag == b"-":
            hunk.old.append(line[1:])
            old_seen += 1
            last = (True, False)
        elif tag == b"+":
            hunk.new.append(line[1:])
            new_seen += 1
            last = (False, True)
        else:
            shown = _text(line.rstrip(b"\r\n"))
            raise _fail(patch, file, f"malformed hunk line: {shown}", hunk.header)
        if old_seen > hunk.old_count or new_seen > new_count:
            raise _fail(patch, file, "hunk has more lines than its header says", hunk.header)


def _safe_path(patch: Path, package_dir: Path, name: bytes, prefix: bytes | None) -> tuple[str, Path]:
    """Strip the a/ or b/ prefix and check that the name stays inside package_dir.

    Returns the name relative to the package, for messages, and the real path.
    """
    shown = _text(name)
    if name.startswith(b"/"):
        raise _fail(patch, shown, "absolute paths are not allowed")
    if prefix is not None:
        if not name.startswith(prefix):
            raise _fail(patch, shown, f"file name does not start with {prefix.decode()!r}")
        name = name[len(prefix) :]
    rel = _text(name)
    if "\0" in rel:
        raise _fail(patch, shown, "file name contains a NUL byte")
    if rel.startswith("/") or os.path.isabs(rel):
        raise _fail(patch, shown, "absolute paths are not allowed")
    parts = [p for p in rel.split("/") if p not in ("", ".")]
    if ".." in parts:
        raise _fail(patch, shown, "paths with `..` are not allowed")
    if not parts:
        raise _fail(patch, shown, "empty file name")
    root = os.path.realpath(package_dir)
    target = os.path.realpath(os.path.join(root, *parts))
    if target == root or os.path.commonpath([root, target]) != root:
        raise _fail(patch, "/".join(parts), "path resolves outside the package directory")
    return "/".join(parts), Path(target)


def _locate(lines: list[bytes], old: list[bytes], guess: int, lo: int, hi: int) -> int | None:
    """Find where old matches in lines, nearest to guess, within [lo, hi]."""
    guess = min(max(guess, lo), hi)
    m = len(old)
    if lines[guess : guess + m] == old:
        return guess
    for d in range(1, max(guess - lo, hi - guess) + 1):
        for pos in (guess + d, guess - d):
            if lo <= pos <= hi and lines[pos : pos + m] == old:
                return pos
    return None


def _apply_hunks(patch: Path, file: str, lines: list[bytes], hunks: list[_Hunk]) -> list[bytes]:
    out: list[bytes] = []
    cursor = 0  # Everything before this line of the original is done.
    offset = 0  # How far the previous hunk was from where it said it was.
    for hunk in hunks:
        m = len(hunk.old)
        # A hunk that removes nothing says which line to insert after.
        stated = max(hunk.old_start - 1 if hunk.old_count > 0 else hunk.old_start, 0)
        hi = len(lines) - m
        pos = _locate(lines, hunk.old, stated + offset, cursor, hi) if cursor <= hi else None
        if pos is None:
            raise _fail(
                patch,
                file,
                "context or removed lines do not match the file (is the package version the one that was patched?)",
                hunk.header,
            )
        out.extend(lines[cursor:pos])
        out.extend(hunk.new)
        cursor = pos + m
        offset = pos - stated
    out.extend(lines[cursor:])
    return out


def _check_mode(patch: Path, file: str, mode: bytes) -> int:
    if mode not in _FILE_MODES:
        raise _fail(patch, file, f"unsupported file mode {_text(mode)} (only 100644 and 100755)")
    return _FILE_MODES[mode]


def _pick(*candidates: tuple[bytes | None, bytes | None]) -> tuple[bytes, bytes | None] | None:
    for name, prefix in candidates:
        if name is not None and name != _DEV_NULL:
            return name, prefix
    return None


def _apply_section(patch: Path, package_dir: Path, section: _FilePatch) -> None:
    is_new = section.new_file_mode is not None or section.minus == _DEV_NULL
    is_delete = section.deleted_file_mode is not None or section.plus == _DEV_NULL
    if is_new and is_delete:
        raise _fail(patch, None, f"section both creates and deletes a file: {section.header}")
    if (section.rename_from is None) != (section.rename_to is None):
        raise _fail(patch, None, f"`rename from` without `rename to`, or the reverse: {section.header}")

    old = _pick((section.rename_from, None), (section.minus, b"a/"), (section.git_old, b"a/"))
    new = _pick((section.rename_to, None), (section.plus, b"b/"), (section.git_new, b"b/"))
    chosen = new if is_new else old if is_delete else old or new
    if chosen is None:
        raise _fail(patch, None, f"cannot tell which file this section is for: {section.header}")

    shown, path = _safe_path(patch, package_dir, *chosen)
    dest = path
    dest_shown = shown
    if not is_new and not is_delete and old is not None and new is not None:
        dest_shown, dest = _safe_path(patch, package_dir, *new)
        if dest != path and section.rename_from is None:
            raise _fail(patch, shown, f"the file is also named {dest_shown} but there is no rename header")

    for mode in (section.new_mode, section.old_mode, section.new_file_mode, section.deleted_file_mode):
        if mode is not None:
            _check_mode(patch, shown, mode)

    try:
        if is_new:
            if os.path.lexists(path):
                raise _fail(patch, shown, "file to create already exists")
            data = b"".join(_apply_hunks(patch, shown, [], section.hunks))
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
            path.chmod(_check_mode(patch, shown, section.new_file_mode or b"100644"))
            return
        if not path.is_file():
            raise _fail(patch, shown, "file to patch does not exist")
        original = path.read_bytes()
        if is_delete:
            if section.hunks and _apply_hunks(patch, shown, _split_lines(original), section.hunks):
                raise _fail(patch, shown, "file to delete still has content after its hunks are applied")
            path.unlink()
            return
        data = original
        if section.hunks:
            data = b"".join(_apply_hunks(patch, shown, _split_lines(original), section.hunks))
        mode = path.stat().st_mode & 0o7777
        if section.new_mode == b"100755":
            mode |= (mode & 0o444) >> 2
        elif section.new_mode is not None:
            mode &= ~0o111
        if dest != path:
            if os.path.lexists(dest):
                raise _fail(patch, dest_shown, f"rename target of {shown} already exists")
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(data)
            dest.chmod(mode)
            path.unlink()
        else:
            if data != original:
                path.write_bytes(data)
            path.chmod(mode)
    except OSError as e:
        raise _fail(patch, shown, e.strerror or str(e))


def apply_patch(patch: Path, package_dir: Path, expected_hash: str | None = None) -> None:
    """Apply a pnpm patch file to the extracted package in package_dir.

    expected_hash is the hash the lockfile recorded for the patch. pnpm 10 and
    later record the lowercase hex sha256 of the patch file; if it is 64 hex
    characters it is checked, and any other value (an older pnpm's format) is
    ignored. Every failure raises ValueError.
    """
    data = patch.read_bytes()
    if expected_hash is not None and _SHA256_RE.fullmatch(expected_hash):
        actual = hashlib.sha256(data).hexdigest()
        if actual != expected_hash.lower():
            raise ValueError(
                f"{patch}: sha256 is {actual} but the lockfile records {expected_hash.lower()}; "
                "the lockfile is out of date with the patch file (run `pnpm install`)"
            )
    for section in _parse_patch(patch, data):
        _apply_section(patch, package_dir, section)
