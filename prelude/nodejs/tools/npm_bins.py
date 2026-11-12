# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

"""Write `node_modules/.bin` directories from npm packages' `bin` declarations.

This does at build time what npm and pnpm do at install time: each bin of each
package becomes a symlink in a `.bin` directory, and with `windows` a `.cmd`
shim sits beside it. The symlinks are relative to the final layout and may
dangle where the directory is written.

Besides the usual `.bin/<bin>` links there is `.bin/.default/<alias>`, one
fixed path per package that has a default bin, whatever that bin is called.
"""

from __future__ import annotations

import json
import os
import posixpath
import re
import stat
from pathlib import Path

_SCOPE = re.compile(r"^@[^/]+/")
_BAD_NAME_CHARS = re.compile(r"[\x00-\x1f:]")


def _read_manifest(package_dir: Path) -> dict:
    try:
        with open(package_dir / "package.json", encoding="utf-8") as f:
            manifest = json.load(f)
    except (FileNotFoundError, NotADirectoryError):
        return {}
    return manifest if isinstance(manifest, dict) else {}


def _unscoped(name: str) -> str:
    return _SCOPE.sub("", name)


def _bin_name(key: str) -> str | None:
    name = re.split(r"[/\\]", key)[-1]
    if name in ("", ".", "..") or _BAD_NAME_CHARS.search(name):
        return None
    return name


def _clamp(path: str) -> str:
    """Normalize `path` as `path.join('/', path).slice(1)` does, so it cannot leave its root."""
    path = path.replace("\\", "/").lstrip("/")
    return posixpath.normpath("/" + path).lstrip("/")


def _normalize(bins: object, name: object) -> dict[str, str]:
    if isinstance(bins, str):
        bins = {_unscoped(name): bins} if isinstance(name, str) else {}
    result: dict[str, str] = {}
    if isinstance(bins, dict):
        for key, value in bins.items():
            if not isinstance(value, str):
                continue
            bin_name = _bin_name(key)
            target = _clamp(value)
            if bin_name is not None and target:
                result[bin_name] = target
    return result


def _directory_bins(package_dir: Path, manifest: dict) -> dict[str, str]:
    directories = manifest.get("directories")
    bin_dir = directories.get("bin") if isinstance(directories, dict) else None
    if not isinstance(bin_dir, str):
        return {}
    rel = _clamp(bin_dir)
    root = package_dir / rel if rel else package_dir
    result: dict[str, str] = {}
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames.sort()
        for filename in sorted(filenames):
            path = Path(dirpath, filename)
            if path.is_file():
                result[filename] = path.relative_to(package_dir).as_posix()
    return result


def package_bins(package_dir: Path) -> dict[str, str]:
    """Return the bins `package_dir/package.json` declares, as `{bin name: path in the package}`.

    Paths are posix, normalized the way npm normalizes them, clamped to stay
    inside the package, and have no leading `./`. A string `bin` is named after
    the package without its scope. Without a `bin`, `directories.bin` names
    every file under it by its basename. Anything else gives `{}`. The result
    is sorted by name.
    """
    manifest = _read_manifest(package_dir)
    if manifest.get("bin") is not None:
        bins = _normalize(manifest["bin"], manifest.get("name"))
    else:
        bins = _directory_bins(package_dir, manifest)
    return dict(sorted(bins.items()))


def default_bin(package_name: str, bins: dict[str, str]) -> str | None:
    """Return the name of the bin npx runs when given only the package, or None.

    That is the package's only bin, or else the bin named after the package
    with its scope removed.
    """
    if len(bins) == 1:
        return next(iter(bins))
    name = _unscoped(package_name)
    return name if name in bins else None


def _shebang_command(first_line: bytes) -> tuple[str, list[str]] | None:
    """Return the program and arguments of a `#!` line, looking through `env`."""
    if not first_line.startswith(b"#!"):
        return None
    words = first_line[2:].decode("utf-8", "replace").split()
    if words and posixpath.basename(words[0]) == "env":
        words = words[1:]
        while words and (words[0].startswith("-") or "=" in words[0]):
            words = words[1:]
    if not words:
        return None
    return posixpath.basename(words[0]), words[1:]


def _cmd_shim(target: str, first_line: bytes) -> str:
    """Return the text of a cmd shim running `target`, a posix path relative to the shim."""
    file = '"%~dp0\\' + target.replace("/", "\\") + '"'
    shebang = _shebang_command(first_line)
    extension = posixpath.splitext(target)[1].lower()
    if extension in (".exe", ".cmd", ".bat") or shebang is None:
        call = "@CALL " if extension in (".cmd", ".bat") else "@"
        body = [f"{call}{file} %*"]
    else:
        program, args = shebang
        arg_text = "".join(f" {a}" for a in args)
        if program in ("node", "node.exe"):
            body = [
                '@IF EXIST "%~dp0\\node.exe" (',
                f'  "%~dp0\\node.exe"{arg_text} {file} %*',
                ") ELSE (",
                f"  @node{arg_text} {file} %*",
                ")",
            ]
        else:
            body = [f"@{program}{arg_text} {file} %*"]
    return "\r\n".join(["@SETLOCAL", *body, "@exit /b %ERRORLEVEL%", ""])


def _first_line(path: Path) -> bytes:
    try:
        with open(path, "rb") as f:
            return f.readline(4096).rstrip(b"\r\n")
    except OSError:
        return b""


def write_bins(
    out: Path, packages: dict[str, Path], rel_root: str, windows: bool
) -> dict[str, str]:
    """Write the `.bin` directory `out` for `packages`, a map of alias to the directory to read it from.

    Each package sits at `<root>/<alias>` in the final layout, and `rel_root` is
    the posix path from `out` to `<root>`. Every bin becomes a relative symlink
    `out/<bin>` to `<rel_root>/<alias>/<path>`. When packages share a bin name,
    the package named like the bin wins (scope removed), else the alias that
    sorts first. Every package with a default bin also gets
    `out/.default/<alias>`. With `windows`, a `.cmd` shim is written beside
    each symlink, from the first line of the target file in the directory given.
    Returns `{bin name: alias}` for the winning packages.
    """
    candidates: dict[str, list[tuple[bool, str, str]]] = {}
    defaults: list[tuple[str, str]] = []
    first_lines: dict[tuple[str, str], bytes] = {}
    for alias in sorted(packages):
        package_dir = packages[alias]
        name = _read_manifest(package_dir).get("name")
        if not isinstance(name, str):
            name = alias
        bins = package_bins(package_dir)
        bins.pop(".default", None)
        for bin_name, path in bins.items():
            candidates.setdefault(bin_name, []).append(
                (_unscoped(name) != bin_name, alias, path)
            )
        default = default_bin(name, bins)
        if default is not None:
            defaults.append((alias, bins[default]))
        if windows:
            for path in bins.values():
                first_lines[(alias, path)] = _first_line(package_dir / path)

    out.mkdir(parents=True, exist_ok=True)
    winners: dict[str, str] = {}
    for bin_name, options in sorted(candidates.items()):
        _, alias, path = min(options)
        winners[bin_name] = alias
        target = f"{rel_root}/{alias}/{path}"
        os.symlink(target, out / bin_name)
        if windows:
            shim = _cmd_shim(target, first_lines[(alias, path)])
            (out / f"{bin_name}.cmd").write_bytes(shim.encode())

    for alias, path in defaults:
        link = out / ".default" / alias
        link.parent.mkdir(parents=True, exist_ok=True)
        target = "../" * (1 + alias.count("/")) + f"{rel_root}/{alias}/{path}"
        os.symlink(target, link)
        if windows:
            shim = _cmd_shim(target, first_lines[(alias, path)])
            link.with_name(link.name + ".cmd").write_bytes(shim.encode())
    return winners


def make_bins_executable(package_dir: Path) -> None:
    """Add execute bits to each existing bin target of the package, as npm does at install.

    Targets that are missing or are not regular files are skipped.
    """
    for path in package_bins(package_dir).values():
        target = package_dir / path
        if target.is_file():
            os.chmod(target, stat.S_IMODE(target.stat().st_mode) | 0o111)
