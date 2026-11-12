# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

from __future__ import annotations

import json
import os
import platform
import sys
import tempfile
import unittest
from pathlib import Path

from npm_lifecycle import install_scripts, run_install_scripts


@unittest.skipIf(sys.platform == "win32", "POSIX shell scripts")
class NpmLifecycleTest(unittest.TestCase):
    def setUp(self) -> None:
        td = tempfile.TemporaryDirectory()
        self.addCleanup(td.cleanup)
        self.root = Path(td.name).resolve()
        self.pkg = self.root / "pkg"
        self.pkg.mkdir()
        self.tmp = self.root / "tmp"
        self.tmp.mkdir()
        self.out = self.root / "out"
        self.out.mkdir()
        self.node_dir = self.root / "nodebin"
        self.node_dir.mkdir()
        self.node = self.node_dir / "node"
        self.node_log = self.root / "node.log"
        self.node.write_text(
            '#!/bin/sh\n'
            'if [ "$1" = "--version" ]; then\n'
            f'  echo "x" >> "{self.root}/version-runs"\n'
            '  echo v24.0.0\n  exit 0\nfi\n'
            f'echo "$@" >> "{self.node_log}"\n'
        )
        self.node.chmod(0o755)

    def write_pkg(self, scripts: dict | None, **extra: object) -> None:
        manifest = {"name": "foo", "version": "1.2.3", **extra}
        if scripts is not None:
            manifest["scripts"] = scripts
        (self.pkg / "package.json").write_text(json.dumps(manifest))

    def run_scripts(
        self,
        bin_dirs: list[Path] | None = None,
        node_gyp: Path | None = None,
        node: str | None = None,
    ) -> None:
        run_install_scripts(
            self.pkg, node or str(self.node), bin_dirs or [], node_gyp, self.tmp
        )

    def read(self, name: str) -> str:
        return (self.out / name).read_text()

    def test_order_and_skipping(self) -> None:
        self.write_pkg(
            {
                "postinstall": f'echo post >> "{self.out}/log"',
                "preinstall": f'echo pre >> "{self.out}/log"',
                "install": f'echo inst >> "{self.out}/log"',
                "prepare": f'echo prepare >> "{self.out}/log"',
            }
        )
        self.run_scripts()
        self.assertEqual(self.read("log"), "pre\ninst\npost\n")

    def test_install_scripts_skips_missing_empty_and_non_string(self) -> None:
        self.write_pkg({"preinstall": "", "install": 5, "postinstall": "b"})
        self.assertEqual(install_scripts(self.pkg), [("postinstall", "b")])
        self.write_pkg(None)
        self.assertEqual(install_scripts(self.pkg), [])
        self.write_pkg({"scripts": "x"})
        self.assertEqual(install_scripts(self.pkg), [])

    def test_implicit_node_gyp(self) -> None:
        self.write_pkg({})
        (self.pkg / "binding.gyp").write_text("{}")
        self.assertEqual(install_scripts(self.pkg), [("install", "node-gyp rebuild")])

    def test_implicit_node_gyp_absent_with_preinstall_or_install(self) -> None:
        (self.pkg / "binding.gyp").write_text("{}")
        self.write_pkg({"preinstall": "true"})
        self.assertEqual(install_scripts(self.pkg), [("preinstall", "true")])
        self.write_pkg({"install": "true"})
        self.assertEqual(install_scripts(self.pkg), [("install", "true")])
        self.write_pkg({"postinstall": "true"})
        self.assertEqual(
            install_scripts(self.pkg),
            [("install", "node-gyp rebuild"), ("postinstall", "true")],
        )

    def test_no_binding_gyp_no_scripts(self) -> None:
        self.write_pkg({})
        self.assertEqual(install_scripts(self.pkg), [])

    def test_env_and_cwd(self) -> None:
        gyp = self.root / "node_modules/node-gyp/bin/node-gyp.js"
        self.write_pkg(
            {
                "install": (
                    f'pwd > "{self.out}/pwd"; '
                    f'env > "{self.out}/env"'
                )
            }
        )
        self.run_scripts(node_gyp=gyp)
        self.assertEqual(self.read("pwd").strip(), str(self.pkg))
        env = dict(
            line.split("=", 1) for line in self.read("env").splitlines() if "=" in line
        )
        self.assertEqual(env["npm_lifecycle_event"], "install")
        self.assertEqual(env["npm_lifecycle_script"].split(";")[0], f'pwd > "{self.out}/pwd"')
        self.assertEqual(env["npm_package_name"], "foo")
        self.assertEqual(env["npm_package_version"], "1.2.3")
        self.assertEqual(env["npm_package_json"], str(self.pkg / "package.json"))
        self.assertEqual(env["npm_node_execpath"], str(self.node))
        self.assertEqual(env["NODE"], str(self.node))
        self.assertEqual(env["npm_config_node_gyp"], str(gyp))
        self.assertEqual(env["INIT_CWD"], str(self.pkg))
        machine = {"x86_64": "x64", "aarch64": "arm64"}.get(
            platform.machine().lower(), platform.machine().lower()
        )
        plat = "linux" if sys.platform.startswith("linux") else sys.platform
        self.assertEqual(
            env["npm_config_user_agent"],
            f"pnpm/10.0.0 npm/? node/v24.0.0 {plat} {machine} (buck2)",
        )

    def test_node_gyp_env_absent_without_node_gyp(self) -> None:
        self.write_pkg({"install": f'env > "{self.out}/env"'})
        self.run_scripts()
        self.assertNotIn("npm_config_node_gyp=", self.read("env"))

    def test_lifecycle_event_per_script(self) -> None:
        self.write_pkg(
            {
                "preinstall": f'echo $npm_lifecycle_event >> "{self.out}/ev"',
                "postinstall": f'echo $npm_lifecycle_event >> "{self.out}/ev"',
            }
        )
        self.run_scripts()
        self.assertEqual(self.read("ev"), "preinstall\npostinstall\n")

    def test_path_order(self) -> None:
        a, b = self.root / "a", self.root / "b"
        gyp = self.root / "gyp.js"
        self.write_pkg({"install": f'echo "$PATH" > "{self.out}/path"'})
        old = os.environ["PATH"]
        self.run_scripts(bin_dirs=[a, b], node_gyp=gyp)
        self.assertEqual(
            self.read("path").strip().split(os.pathsep),
            [
                str(a),
                str(b),
                str(self.tmp / "node-gyp-bin"),
                str(self.node_dir),
                *old.split(os.pathsep),
            ],
        )

    def test_path_with_bare_node_has_no_node_dir(self) -> None:
        self.write_pkg({"install": f'echo "$PATH" > "{self.out}/path"'})
        old = os.environ["PATH"]
        # A bare name is found through the inherited PATH.
        os.environ["PATH"] = f"{self.node_dir}{os.pathsep}{old}"
        try:
            self.run_scripts(node="node")
        finally:
            os.environ["PATH"] = old
        self.assertEqual(
            self.read("path").strip(), f"{self.node_dir}{os.pathsep}{old}"
        )

    def test_node_gyp_shim(self) -> None:
        gyp = self.root / "it's/node-gyp.js"
        self.write_pkg({"install": "node-gyp rebuild --foo 'a b'"})
        self.run_scripts(node_gyp=gyp)
        self.assertEqual(self.node_log.read_text(), f"{gyp} rebuild --foo a b\n")
        shim = self.tmp / "node-gyp-bin"
        self.assertTrue(os.access(shim / "node-gyp", os.X_OK))
        cmd = (shim / "node-gyp.cmd").read_text()
        self.assertIn(f'@"{self.node}" "{gyp}" %*', cmd)

    def test_failure_message(self) -> None:
        self.write_pkg({"postinstall": "exit 3"})
        with self.assertRaises(RuntimeError) as cm:
            self.run_scripts()
        msg = str(cm.exception)
        for part in ("foo@1.2.3", "postinstall", "exit 3", "3"):
            self.assertIn(part, msg)
        self.assertNotIn("node-gyp", msg)

    def test_failure_stops_later_scripts(self) -> None:
        self.write_pkg({"install": "exit 1", "postinstall": f'touch "{self.out}/x"'})
        with self.assertRaises(RuntimeError):
            self.run_scripts()
        self.assertFalse((self.out / "x").exists())

    def test_failure_node_gyp_hint(self) -> None:
        self.write_pkg({})
        (self.pkg / "binding.gyp").write_text("{}")
        with self.assertRaises(RuntimeError) as cm:
            self.run_scripts()
        msg = str(cm.exception)
        self.assertIn("node-gyp rebuild", msg)
        self.assertIn("node-gyp is not available", msg)
        self.assertIn("workspace root", msg)

    def test_failure_no_hint_when_node_gyp_given(self) -> None:
        self.write_pkg({"install": "node-gyp rebuild; exit 2"})
        with self.assertRaises(RuntimeError) as cm:
            self.run_scripts(node_gyp=self.root / "gyp.js")
        self.assertNotIn("not available", str(cm.exception))

    def test_no_scripts_does_not_run_node(self) -> None:
        self.write_pkg({})
        self.run_scripts()
        self.assertFalse((self.root / "version-runs").exists())
        self.assertFalse((self.tmp / "node-gyp-bin").exists())

    def test_node_version_runs_once(self) -> None:
        self.write_pkg({"preinstall": "true", "install": "true", "postinstall": "true"})
        self.run_scripts()
        self.assertEqual((self.root / "version-runs").read_text(), "x\n")


if __name__ == "__main__":
    unittest.main()
