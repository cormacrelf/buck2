# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# pyre-strict

# What `buck2 dev` does for the different ways a file can change on disk: editors' saves,
# deletes, renames, directories, permissions, symlinks, many files at once, and files that
# `buck2 dev` should not see. Each test states the behaviour it requires. Some fail against the
# current `buck2 dev`: they describe behaviour it should have. They use the fixture of
# `test_dev`.

import asyncio
import os
import shutil

from buck2.tests.core.common.dev.dev_util import (
    APP,
    assert_kept,
    changed,
    dev_test,
    edit_target,
    files,
    read,
    site,
    start_dev,
    write,
)
from buck2.tests.e2e_util.api.buck import Buck
from buck2.tests.e2e_util.buck_workspace import BuckTestCase


class DevFilesTest(BuckTestCase):
    @dev_test()
    async def test_save_through_a_temporary_file(self, buck: Buck) -> None:
        """An editor's save that writes a temporary file and renames it over the file: one
        build, with only the file in `files`."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            temp = buck.cwd / "app/.main.txt.tmp"
            temp.write_text("app v2\n")
            os.replace(temp, buck.cwd / "app/main.txt")
            building, built = await dev.expect("building", "built")
            assert files(buck, building) == ["app/main.txt"]
            assert site(built) == "app v2\n"
            assert built["pid"] == first["pid"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_save_without_a_change_through_a_temporary_file(
        self, buck: Buck
    ) -> None:
        """The same save with the same content: nothing is built."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            temp = buck.cwd / "app/.main.txt.tmp"
            temp.write_text("app v1\n")
            os.replace(temp, buck.cwd / "app/main.txt")
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_truncate_then_write(self, buck: Buck) -> None:
        """A save that truncates the file and then writes it, in two steps: one build, of the
        final content."""
        dev = await start_dev(buck, "//app:all")
        try:
            await dev.start()
            with open(buck.cwd / "app/main.txt", "w") as f:
                f.flush()
                os.fsync(f.fileno())
                await asyncio.sleep(0.05)
                f.write("app v2\n")
            _, built = await dev.expect("building", "built")
            assert site(built) == "app v2\n"
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_emptied_file(self, buck: Buck) -> None:
        """A file emptied: it builds, and the output changed."""
        dev = await start_dev(buck, "//app:all")
        try:
            await dev.start()
            write(buck, "app/main.txt", "")
            _, built = await dev.expect("building", "built")
            assert site(built) == ""
            assert changed(built) == {"site": True}
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_delete_and_recreate_at_once(self, buck: Buck) -> None:
        """A file deleted and created again with new content at once: one build."""
        dev = await start_dev(buck, "//app:all")
        try:
            await dev.start()
            os.unlink(buck.cwd / "app/main.txt")
            write(buck, "app/main.txt", "app v2\n")
            building, built = await dev.expect("building", "built")
            assert files(buck, building) == ["app/main.txt"]
            assert site(built) == "app v2\n"
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_source_file_deleted_then_recreated(self, buck: Buck) -> None:
        """A source file deleted: loading the build file fails, which sends `failed` without
        `building`, and the server keeps running. Creating it again builds."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            os.unlink(buck.cwd / "app/main.txt")
            (failed,) = await dev.expect("failed")
            assert "Error coercing attribute `srcs`" in failed["error"], failed["error"]
            await dev.quiet()
            assert_kept(dev, first)

            write(buck, "app/main.txt", "app v2\n")
            _, built = await dev.expect("building", "built")
            assert site(built) == "app v2\n"
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_source_file_renamed_away_and_back(self, buck: Buck) -> None:
        """A source file renamed away: `failed`. Renamed back: `graph_changed`, which tells the
        server that the analysis works again."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            os.rename(buck.cwd / "app/main.txt", buck.cwd / "app/main.txt.bak")
            await dev.expect("failed")
            await dev.wait_until_quiet()
            os.rename(buck.cwd / "app/main.txt.bak", buck.cwd / "app/main.txt")
            await dev.expect("graph_changed", timeout=10)
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_globbed_directory_moved_away_and_back(self, buck: Buck) -> None:
        """The directory that a `glob` matches moved away: `graph_changed` and an empty build.
        Moved back: `graph_changed` and the old output."""
        dev = await start_dev(buck, "//app:pages")
        try:
            first = await dev.start()
            os.rename(buck.cwd / "app/pages", buck.cwd / "app/pages_moved")
            _, building, built = await dev.expect("graph_changed", "building", "built")
            assert site(built) == ""
            os.rename(buck.cwd / "app/pages_moved", buck.cwd / "app/pages")
            _, _, built = await dev.expect("graph_changed", "building", "built")
            assert site(built) == "page a\n"
            assert built["pid"] == first["pid"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_new_package_boundary_inside_a_glob(self, buck: Buck) -> None:
        """A build file created in a globbed directory makes it a package of its own, so the
        `glob` no longer matches its files; deleting the build file brings them back."""
        dev = await start_dev(buck, "//app:pages")
        try:
            await dev.start()
            write(buck, "app/pages/TARGETS.fixture", "")
            _, _, built = await dev.expect("graph_changed", "building", "built")
            assert site(built) == ""
            os.unlink(buck.cwd / "app/pages/TARGETS.fixture")
            _, _, built = await dev.expect("graph_changed", "building", "built")
            assert site(built) == "page a\n"
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_file_that_the_glob_does_not_match(self, buck: Buck) -> None:
        """A new file in a globbed directory that the `glob` does not match: nothing."""
        dev = await start_dev(buck, "//app:pages")
        try:
            first = await dev.start()
            write(buck, "app/pages/notes.md", "notes\n")
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_source_file_replaced_by_a_directory(self, buck: Buck) -> None:
        """A source file replaced by a directory: loading the build file fails, and the server
        keeps running. A file again builds."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            os.unlink(buck.cwd / "app/main.txt")
            os.mkdir(buck.cwd / "app/main.txt")
            (failed,) = await dev.expect("failed")
            assert "Error coercing attribute `srcs`" in failed["error"], failed["error"]
            await dev.quiet()
            assert_kept(dev, first)

            os.rmdir(buck.cwd / "app/main.txt")
            write(buck, "app/main.txt", "app v2\n")
            _, built = await dev.expect("building", "built")
            assert site(built) == "app v2\n"
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_chmod_only(self, buck: Buck) -> None:
        """A source file made executable, with the same content: it builds, and the output is
        unchanged."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            os.chmod(buck.cwd / "app/main.txt", 0o755)
            building, built = await dev.expect("building", "built")
            assert files(buck, building) == ["app/main.txt"]
            assert changed(built) == {"site": False}
            assert built["pid"] == first["pid"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_symlink_retargeted(self, buck: Buck) -> None:
        """A source file replaced by a symlink to another file, then the symlink pointed at a
        third: each builds the content it points at."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            # Files outside the graph: nothing.
            write(buck, "app/target_a.txt", "target a\n")
            write(buck, "app/target_b.txt", "target b\n")
            await dev.quiet(2)

            main = buck.cwd / "app/main.txt"
            os.unlink(main)
            os.symlink("target_a.txt", main)
            _, built = await dev.expect("building", "built")
            assert site(built) == "target a\n"

            os.unlink(main)
            os.symlink("target_b.txt", main)
            _, built = await dev.expect("building", "built")
            assert site(built) == "target b\n"
            assert built["pid"] == first["pid"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_symlink_target_edited(self, buck: Buck) -> None:
        """A source file that is a symlink, and the file it points at edited: it builds the new
        content."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            write(buck, "app/target_a.txt", "target a\n")
            main = buck.cwd / "app/main.txt"
            os.unlink(main)
            os.symlink("target_a.txt", main)
            _, built = await dev.expect("building", "built")
            assert site(built) == "target a\n"

            write(buck, "app/target_a.txt", "target a2\n")
            _, built = await dev.expect("building", "built", timeout=10)
            assert site(built) == "target a2\n"
            assert built["pid"] == first["pid"]
        finally:
            await dev.stop()

    @dev_test()
    async def test_many_files_at_once(self, buck: Buck) -> None:
        """Many files at once, as when switching branches: a source file, a dependency's file,
        and the build file. One `graph_changed`, and one build of all of it."""
        dev = await start_dev(buck, "//app:with_dep")
        try:
            first = await dev.start()
            write(buck, "app/main.txt", "app v2\n")
            write(buck, "lib/lib.txt", "lib v2\n")
            edit_target(
                buck,
                APP,
                "with_dep",
                'srcs = ["main.txt"],',
                'srcs = ["main.txt", "other.txt"],',
            )
            _, building, built = await dev.expect("graph_changed", "building", "built")
            assert files(buck, building) == ["app/main.txt", "lib/lib.txt"]
            assert site(built) == "lib v2\napp v2\nother v1\n"
            assert built["pid"] == first["pid"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_rapid_saves(self, buck: Buck) -> None:
        """Ten saves 50 ms apart, less than the debounce: one build, of the last content."""
        dev = await start_dev(buck, "//app:all")
        try:
            await dev.start()
            for i in range(10):
                write(buck, "app/main.txt", f"app v{i + 2}\n")
                await asyncio.sleep(0.05)
            _, built = await dev.expect("building", "built")
            assert site(built) == "app v11\n"
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test(extra_buck_config={"project": {"ignore": "app/ignored"}})
    async def test_buck_out_and_ignored_files(self, buck: Buck) -> None:
        """Files under `buck-out` and under a directory that the project ignores: nothing."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            write(buck, "buck-out/dev_test.txt", "x\n")
            write(buck, "app/ignored/main.txt", "x\n")
            await dev.quiet()
            assert_kept(dev, first)

            write(buck, "app/main.txt", "app v2\n")
            building, _ = await dev.expect("building", "built")
            assert files(buck, building) == ["app/main.txt"]
        finally:
            await dev.stop()

    @dev_test()
    async def test_buckconfig_change(self, buck: Buck) -> None:
        """A `.buckconfig` change that changes the target's sources."""
        dev = await start_dev(buck, "//app:configured")
        try:
            first = await dev.start()
            write(
                buck,
                ".buckconfig",
                read(buck, ".buckconfig") + "\n[dev_test]\n  src = other.txt\n",
            )
            # dev.md says that `buck2 dev` does not act on `.buckconfig` changes, but the
            # daemon reads it again when `buck2 dev` asks what changed, and the target changed.
            _, building, built = await dev.expect("graph_changed", "building", "built")
            assert site(built) == "other v1\n"
            assert built["pid"] == first["pid"]

            write(buck, "app/main.txt", "app v2\n")
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_new_unrelated_directory_tree(self, buck: Buck) -> None:
        """A directory tree of files that no target uses, created and removed: nothing."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            for i in range(20):
                write(buck, f"scratch/d{i % 4}/f{i}.txt", f"{i}\n")
            await dev.quiet(2)
            shutil.rmtree(buck.cwd / "scratch")
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()
