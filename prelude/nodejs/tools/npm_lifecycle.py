# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

"""Run an npm package's install lifecycle scripts the way npm and pnpm do.

Runs `preinstall`, `install` and `postinstall` from the package's
`package.json`, in that order, with the package directory as the working
directory. It runs after the package is extracted and its dependencies are
linked beside it.
"""

from __future__ import annotations

import json
import os
import platform
import subprocess
import sys
from pathlib import Path

EVENTS = ("preinstall", "install", "postinstall")

_MACHINES = {
    "x86_64": "x64",
    "amd64": "x64",
    "aarch64": "arm64",
    "arm64": "arm64",
    "i386": "ia32",
    "i686": "ia32",
    "x86": "ia32",
    "armv7l": "arm",
    "armv8l": "arm",
}


def _read_package_json(package_dir: Path) -> dict:
    with open(package_dir / "package.json", encoding="utf-8") as f:
        data = json.load(f)
    return data if isinstance(data, dict) else {}


def install_scripts(package_dir: Path) -> list[tuple[str, str]]:
    """Return the `(event, script)` pairs to run, in run order.

    Missing, empty and non-string scripts are skipped. As npm does, a package
    with a `binding.gyp` and neither an `install` nor a `preinstall` script
    gets `node-gyp rebuild` as its `install` script.
    """
    scripts = _read_package_json(package_dir).get("scripts")
    if not isinstance(scripts, dict):
        scripts = {}
    found = {
        event: scripts[event]
        for event in EVENTS
        if isinstance(scripts.get(event), str) and scripts[event]
    }
    if (
        "install" not in found
        and "preinstall" not in found
        and (package_dir / "binding.gyp").exists()
    ):
        found["install"] = "node-gyp rebuild"
    return [(event, found[event]) for event in EVENTS if event in found]


def _sh_quote(s: str) -> str:
    return "'" + s.replace("'", "'\\''") + "'"


def _cmd_quote(s: str) -> str:
    return '"' + s.replace("%", "%%").replace('"', '""') + '"'


def _write_node_gyp_shim(tmp: Path, node: str, node_gyp: Path) -> Path:
    """Create `node-gyp` and `node-gyp.cmd` launchers in `tmp/node-gyp-bin`."""
    shim_dir = tmp / "node-gyp-bin"
    shim_dir.mkdir(parents=True, exist_ok=True)
    node_abs = os.path.abspath(node) if os.path.dirname(node) else node
    gyp_abs = os.path.abspath(node_gyp)
    sh = shim_dir / "node-gyp"
    sh.write_bytes(
        f'#!/bin/sh\nexec {_sh_quote(node_abs)} {_sh_quote(gyp_abs)} "$@"\n'.encode()
    )
    sh.chmod(0o755)
    (shim_dir / "node-gyp.cmd").write_bytes(
        f"@{_cmd_quote(node_abs)} {_cmd_quote(gyp_abs)} %*\r\n".encode()
    )
    return shim_dir


def _platform_name() -> str:
    if sys.platform.startswith("linux"):
        return "linux"
    return sys.platform


def _arch_name() -> str:
    machine = platform.machine().lower()
    return _MACHINES.get(machine, machine)


def _node_version(node: str) -> str:
    out = subprocess.run(
        [node, "--version"],
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    return out.stdout.strip()


def _set_path(
    env: dict[str, str], bin_dirs: list[Path], shim_dir: Path | None, node: str
) -> None:
    key = "PATH"
    if sys.platform == "win32":
        key = next((k for k in env if k.upper() == "PATH"), "PATH")
    parts = [os.path.abspath(d) for d in bin_dirs]
    if shim_dir is not None:
        parts.append(str(shim_dir))
    node_dir = os.path.dirname(node)
    if node_dir:
        parts.append(os.path.abspath(node_dir))
    inherited = env.get(key)
    if inherited:
        parts.append(inherited)
    env[key] = os.pathsep.join(parts)


def run_install_scripts(
    package_dir: Path,
    node: str,
    bin_dirs: list[Path],
    node_gyp: Path | None,
    tmp: Path,
) -> None:
    """Run the package's install scripts in order with `package_dir` as cwd.

    `node` is the node executable, a path or a bare command name. `bin_dirs`
    and the directory of `node` are put on PATH, and so is a `node-gyp` shim
    written under `tmp` when `node_gyp` (node-gyp's JS entry) is given. Output
    passes through. Raises `RuntimeError` naming the package, event, script
    and exit code when a script fails.
    """
    scripts = install_scripts(package_dir)
    if not scripts:
        return
    package_dir = package_dir.absolute()
    manifest = _read_package_json(package_dir)
    name = str(manifest.get("name", ""))
    version = str(manifest.get("version", ""))

    env = dict(os.environ)
    shim_dir = _write_node_gyp_shim(tmp, node, node_gyp) if node_gyp else None
    _set_path(env, bin_dirs, shim_dir, node)
    env.update(
        {
            "npm_package_name": name,
            "npm_package_version": version,
            "npm_package_json": str(package_dir / "package.json"),
            "npm_node_execpath": node,
            "NODE": node,
            "INIT_CWD": str(package_dir),
            "npm_config_user_agent": (
                f"pnpm/10.0.0 npm/? node/{_node_version(node)} "
                f"{_platform_name()} {_arch_name()} (buck2)"
            ),
        }
    )
    if node_gyp is not None:
        env["npm_config_node_gyp"] = str(node_gyp)

    if sys.platform == "win32":
        shell = [os.environ.get("ComSpec", "cmd.exe"), "/d", "/s", "/c"]
    else:
        shell = ["sh", "-c"]

    for event, script in scripts:
        env["npm_lifecycle_event"] = event
        env["npm_lifecycle_script"] = script
        code = subprocess.run([*shell, script], cwd=package_dir, env=env).returncode
        if code != 0:
            msg = (
                f"{name}@{version}: {event} script `{script}` "
                f"failed with exit code {code}"
            )
            if node_gyp is None and "node-gyp" in script:
                msg += (
                    "; node-gyp is not available to install scripts unless "
                    "the workspace root lists `node-gyp` as a dependency"
                )
            raise RuntimeError(msg)


def main(
    package_dir: Path,
    node: str,
    bin_dirs: list[Path],
    node_gyp: Path | None,
    tmp: Path,
) -> int:
    """Run the install scripts, print a failure to stderr, and return an exit status."""
    try:
        run_install_scripts(package_dir, node, bin_dirs, node_gyp, tmp)
    except RuntimeError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    return 0
