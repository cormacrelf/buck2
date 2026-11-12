# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

from __future__ import annotations

import itertools
import json
import os
import stat
import tempfile
import unittest
from pathlib import Path

import npm_bins

HAS_SYMLINKS = hasattr(os, "symlink")


def make_package(
    root: Path,
    manifest: dict | None,
    files: dict[str, str] | None = None,
) -> Path:
    root.mkdir(parents=True, exist_ok=True)
    if manifest is not None:
        (root / "package.json").write_text(json.dumps(manifest))
    for name, text in (files or {}).items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
    return root


class TempDirTest(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = Path(tmp.name).resolve()
        self.counter = itertools.count()


class PackageBinsTest(TempDirTest):
    def bins(self, manifest: dict | None, files: dict[str, str] | None = None):
        return npm_bins.package_bins(make_package(self.tmp / "pkg", manifest, files))

    def test_no_package_json(self) -> None:
        self.assertEqual(self.bins(None), {})

    def test_no_bin(self) -> None:
        self.assertEqual(self.bins({"name": "a"}), {})

    def test_string_bin(self) -> None:
        self.assertEqual(
            self.bins({"name": "a", "bin": "./cli.js"}), {"a": "cli.js"}
        )

    def test_string_bin_strips_scope(self) -> None:
        self.assertEqual(
            self.bins({"name": "@s/a", "bin": "bin/a.js"}), {"a": "bin/a.js"}
        )

    def test_string_bin_without_name(self) -> None:
        self.assertEqual(self.bins({"bin": "cli.js"}), {})

    def test_object_bin_sorted(self) -> None:
        bins = self.bins({"bin": {"z": "z.js", "a": "./a.js"}})
        self.assertEqual(list(bins.items()), [("a", "a.js"), ("z", "z.js")])

    def test_object_bin_name_is_last_segment(self) -> None:
        bins = self.bins({"bin": {"x/y": "a.js", "p\\q": "b.js", "@s/r": "c.js"}})
        self.assertEqual(bins, {"y": "a.js", "q": "b.js", "r": "c.js"})

    def test_object_bin_skips_bad_names(self) -> None:
        bins = self.bins(
            {
                "bin": {
                    "": "a.js",
                    ".": "a.js",
                    "..": "a.js",
                    "x/": "a.js",
                    "../..": "a.js",
                    "a:b": "a.js",
                    "ok": "ok.js",
                }
            }
        )
        self.assertEqual(bins, {"ok": "ok.js"})

    def test_object_bin_value_clamped(self) -> None:
        bins = self.bins(
            {
                "bin": {
                    "a": "../../etc/passwd",
                    "b": "/abs/b.js",
                    "c": "x/../y/./c.js",
                    "d": "dir\\d.js",
                    "e": "//e.js",
                    "f": "./",
                    "g": "..",
                }
            }
        )
        self.assertEqual(
            bins,
            {
                "a": "etc/passwd",
                "b": "abs/b.js",
                "c": "y/c.js",
                "d": "dir/d.js",
                "e": "e.js",
            },
        )

    def test_non_string_values_ignored(self) -> None:
        self.assertEqual(self.bins({"bin": {"a": 1, "b": None, "c": "c.js"}}), {"c": "c.js"})
        self.assertEqual(self.bins({"name": "a", "bin": 5}), {})
        self.assertEqual(self.bins({"name": 5, "bin": "x.js"}), {})
        self.assertEqual(self.bins({"directories": {"bin": 5}}), {})
        self.assertEqual(self.bins({"directories": 5}), {})

    def test_directories_bin(self) -> None:
        bins = self.bins(
            {"directories": {"bin": "./cmds"}},
            {"cmds/b": "", "cmds/a": "", "cmds/sub/c": "", "other/d": ""},
        )
        self.assertEqual(bins, {"a": "cmds/a", "b": "cmds/b", "c": "cmds/sub/c"})

    def test_directories_bin_clamped(self) -> None:
        make_package(self.tmp / "outside", None, {"x": ""})
        bins = self.bins({"directories": {"bin": "../outside"}}, {"outside/y": ""})
        self.assertEqual(bins, {"y": "outside/y"})

    def test_bin_wins_over_directories(self) -> None:
        bins = self.bins(
            {"bin": {"a": "a.js"}, "directories": {"bin": "cmds"}}, {"cmds/b": ""}
        )
        self.assertEqual(bins, {"a": "a.js"})

    def test_missing_directories_bin(self) -> None:
        self.assertEqual(self.bins({"directories": {"bin": "nope"}}), {})

    @unittest.skipUnless(HAS_SYMLINKS, "no symlinks")
    def test_follows_package_dir_symlink(self) -> None:
        real = make_package(self.tmp / "real", {"name": "a", "bin": "a.js"})
        link = self.tmp / "link"
        os.symlink(real, link)
        self.assertEqual(npm_bins.package_bins(link), {"a": "a.js"})


class DefaultBinTest(unittest.TestCase):
    def test_only_bin(self) -> None:
        self.assertEqual(npm_bins.default_bin("pkg", {"other": "o.js"}), "other")

    def test_named_after_package(self) -> None:
        bins = {"a": "a.js", "pkg": "p.js"}
        self.assertEqual(npm_bins.default_bin("pkg", bins), "pkg")

    def test_named_after_scoped_package(self) -> None:
        bins = {"a": "a.js", "pkg": "p.js"}
        self.assertEqual(npm_bins.default_bin("@s/pkg", bins), "pkg")

    def test_none(self) -> None:
        self.assertIsNone(npm_bins.default_bin("pkg", {"a": "a.js", "b": "b.js"}))
        self.assertIsNone(npm_bins.default_bin("pkg", {}))


@unittest.skipUnless(HAS_SYMLINKS, "no symlinks")
class WriteBinsTest(TempDirTest):
    def pkg(self, name: str, bins: dict[str, str], files: dict[str, str] | None = None):
        files = files if files is not None else {p: "" for p in bins.values()}
        return make_package(
            self.tmp / "src" / name, {"name": name, "bin": bins}, files
        )

    def test_links(self) -> None:
        out = self.tmp / "nm" / ".bin"
        winners = npm_bins.write_bins(
            out, {"a": self.pkg("a", {"x": "x.js", "y": "bin/y.js"})}, "..", False
        )
        self.assertEqual(winners, {"x": "a", "y": "a"})
        self.assertEqual(os.readlink(out / "x"), "../a/x.js")
        self.assertEqual(os.readlink(out / "y"), "../a/bin/y.js")
        self.assertEqual(sorted(p.name for p in out.iterdir()), ["x", "y"])

    def test_rel_root_depths(self) -> None:
        for rel_root, depth in [("..", 1), ("../../..", 3)]:
            out = self.tmp / f"d{depth}" / ".bin"
            npm_bins.write_bins(
                out, {"a": self.pkg("a", {"x": "x.js"})}, rel_root, False
            )
            self.assertEqual(os.readlink(out / "x"), f"{rel_root}/a/x.js")

    def test_empty_contributes_nothing(self) -> None:
        out = self.tmp / "nm" / ".bin"
        packages = {
            "missing": self.tmp / "nowhere",
            "nojson": make_package(self.tmp / "nojson", None),
            "nobins": make_package(self.tmp / "nobins", {"name": "nobins"}),
        }
        self.assertEqual(npm_bins.write_bins(out, packages, "..", True), {})
        self.assertEqual(list(out.iterdir()), [])

    def test_collision_name_match_wins(self) -> None:
        out = self.tmp / "nm" / ".bin"
        packages = {
            "a": self.pkg("a", {"tool": "a.js"}),
            "@s/tool": self.pkg("tool", {"tool": "t.js"}),
            "b": self.pkg("b", {"tool": "b.js"}),
        }
        winners = npm_bins.write_bins(out, packages, "..", False)
        self.assertEqual(winners, {"tool": "@s/tool"})
        self.assertEqual(os.readlink(out / "tool"), "../@s/tool/t.js")

    def test_collision_first_alias_wins(self) -> None:
        out = self.tmp / "nm" / ".bin"
        packages = {
            "b": self.pkg("b", {"tool": "b.js"}),
            "a": self.pkg("a", {"tool": "a.js"}),
        }
        winners = npm_bins.write_bins(out, packages, "..", False)
        self.assertEqual(winners, {"tool": "a"})
        self.assertEqual(os.readlink(out / "tool"), "../a/a.js")

    def test_default_links(self) -> None:
        out = self.tmp / "nm" / ".bin"
        packages = {
            "a": self.pkg("a", {"run": "run.js"}),
            "b": self.pkg("b", {"b": "b.js", "extra": "e.js"}),
            "c": self.pkg("c", {"p": "p.js", "q": "q.js"}),
        }
        npm_bins.write_bins(out, packages, "..", False)
        self.assertEqual(os.readlink(out / ".default" / "a"), "../../a/run.js")
        self.assertEqual(os.readlink(out / ".default" / "b"), "../../b/b.js")
        self.assertFalse(os.path.lexists(out / ".default" / "c"))

    def test_scoped_alias_default(self) -> None:
        out = self.tmp / "nm" / ".bin"
        packages = {"@s/x": self.pkg("@s/x", {"x": "cli.js"})}
        npm_bins.write_bins(out, packages, "..", False)
        self.assertEqual(os.readlink(out / "x"), "../@s/x/cli.js")
        self.assertEqual(
            os.readlink(out / ".default" / "@s" / "x"), "../../../@s/x/cli.js"
        )

    def test_resolves_in_realistic_layout(self) -> None:
        for nested in (False, True):
            top = self.tmp / ("nested" if nested else "flat") / "node_modules"
            if nested:
                bin_dir = top / "host" / "node_modules" / ".bin"
                rel_root = "../../.."
                (top / "host" / "node_modules").mkdir(parents=True)
            else:
                bin_dir = top / ".bin"
                rel_root = ".."
                top.mkdir(parents=True)
            # `../../..` leads from the nested `.bin` to the top `node_modules`.
            root = top
            a = make_package(
                root / "a", {"name": "a", "bin": "bin/a.js"}, {"bin/a.js": ""}
            )
            sx = make_package(
                root / "@s" / "x",
                {"name": "@s/x", "bin": {"x": "x.js"}},
                {"x.js": ""},
            )
            # A package dir read from a different place than where it is linked.
            elsewhere = self.pkg("elsewhere", {"e": "e.js"})
            os.symlink(elsewhere, root / "e")
            npm_bins.write_bins(
                bin_dir, {"a": a, "@s/x": sx, "e": elsewhere}, rel_root, False
            )
            self.assertEqual((bin_dir / "a").resolve(), (a / "bin" / "a.js").resolve())
            self.assertEqual((bin_dir / "x").resolve(), (sx / "x.js").resolve())
            self.assertEqual(
                (bin_dir / ".default" / "@s" / "x").resolve(), (sx / "x.js").resolve()
            )
            self.assertEqual(
                (bin_dir / ".default" / "a").resolve(), (a / "bin" / "a.js").resolve()
            )
            self.assertEqual(
                (bin_dir / "e").resolve(), (elsewhere / "e.js").resolve()
            )

    def shims(self, files: dict[str, str], target: str = "t") -> tuple[str, str]:
        pkg = self.pkg("p", {"p": target}, files)
        out = self.tmp / f"nm{next(self.counter)}" / ".bin"
        npm_bins.write_bins(out, {"p": pkg}, "..", True)
        return (
            (out / "p.cmd").read_bytes().decode(),
            (out / ".default" / "p.cmd").read_bytes().decode(),
        )

    def test_cmd_shim_node(self) -> None:
        for line in ("#!/usr/bin/env node", "#!/usr/bin/node", "#!/usr/bin/env -S node --no-warnings"):
            shim, default = self.shims({"t": line + "\n"})
            args = " --no-warnings" if "--no-warnings" in line else ""
            self.assertEqual(
                shim,
                "@SETLOCAL\r\n"
                '@IF EXIST "%~dp0\\node.exe" (\r\n'
                f'  "%~dp0\\node.exe"{args} "%~dp0\\..\\p\\t" %*\r\n'
                ") ELSE (\r\n"
                f'  @node{args} "%~dp0\\..\\p\\t" %*\r\n'
                ")\r\n"
                "@exit /b %ERRORLEVEL%\r\n",
            )
            self.assertIn('"%~dp0\\..\\..\\p\\t" %*', default)

    def test_cmd_shim_sh(self) -> None:
        shim, _ = self.shims({"bin/t.sh": "#!/bin/sh\necho hi\n"}, "bin/t.sh")
        self.assertEqual(
            shim,
            '@SETLOCAL\r\n@sh "%~dp0\\..\\p\\bin\\t.sh" %*\r\n@exit /b %ERRORLEVEL%\r\n',
        )

    def test_cmd_shim_env_other_program(self) -> None:
        shim, _ = self.shims({"t": "#!/usr/bin/env python3\n"})
        self.assertIn('@python3 "%~dp0\\..\\p\\t" %*', shim)

    def test_cmd_shim_no_shebang(self) -> None:
        shim, _ = self.shims({"t": "echo hi\n"})
        self.assertEqual(
            shim, '@SETLOCAL\r\n@"%~dp0\\..\\p\\t" %*\r\n@exit /b %ERRORLEVEL%\r\n'
        )

    def test_cmd_shim_exe(self) -> None:
        shim, _ = self.shims({"t.exe": "#!/usr/bin/env node\n"}, "t.exe")
        self.assertIn('@"%~dp0\\..\\p\\t.exe" %*', shim)
        self.assertNotIn("node", shim)

    def test_cmd_shim_missing_target(self) -> None:
        shim, _ = self.shims({})
        self.assertIn('@"%~dp0\\..\\p\\t" %*', shim)

    def test_cmd_shim_scoped_default(self) -> None:
        pkg = make_package(
            self.tmp / "s",
            {"name": "@s/x", "bin": {"x": "x.js"}},
            {"x.js": "#!/usr/bin/env node\n"},
        )
        out = self.tmp / "nm" / ".bin"
        npm_bins.write_bins(out, {"@s/x": pkg}, "..", True)
        shim = (out / ".default" / "@s" / "x.cmd").read_text()
        self.assertIn('"%~dp0\\..\\..\\..\\@s\\x\\x.js" %*', shim)

    def test_no_shims_without_windows(self) -> None:
        out = self.tmp / "nm" / ".bin"
        npm_bins.write_bins(out, {"a": self.pkg("a", {"a": "a.js"})}, "..", False)
        self.assertFalse(any(p.suffix == ".cmd" for p in out.rglob("*")))


class MakeBinsExecutableTest(TempDirTest):
    @unittest.skipIf(os.name == "nt", "no mode bits")
    def test_chmod(self) -> None:
        pkg = make_package(
            self.tmp / "p",
            {"bin": {"a": "a.js", "b": "gone.js", "c": "dir"}},
            {"a.js": "", "dir/f": ""},
        )
        (pkg / "a.js").chmod(0o600)
        (pkg / "dir").chmod(0o700)
        npm_bins.make_bins_executable(pkg)
        self.assertEqual(stat.S_IMODE((pkg / "a.js").stat().st_mode), 0o711)
        self.assertEqual(stat.S_IMODE((pkg / "dir").stat().st_mode), 0o700)
        self.assertFalse((pkg / "gone.js").exists())

    def test_no_package_json(self) -> None:
        npm_bins.make_bins_executable(self.tmp)


if __name__ == "__main__":
    unittest.main()
