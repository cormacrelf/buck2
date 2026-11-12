# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# pyre-strict

# What `buck2 dev` does when the graph of its target breaks or changes while it runs: build
# files, `.bzl` files, dependencies and the `DevInfo` itself. Each test states the behaviour it
# requires. Some fail against the current `buck2 dev`: they describe behaviour it should have.
# They use the fixture of `test_dev`.

import os
from typing import Callable, Set

from buck2.tests.core.common.dev.dev_util import (
    APP,
    assert_kept,
    changed,
    contents,
    dev_test,
    edit_target,
    files,
    LIB,
    read,
    remove_target,
    site,
    start_dev,
    summary,
    write,
)
from buck2.tests.e2e_util.api.buck import Buck
from buck2.tests.e2e_util.buck_workspace import BuckTestCase

# How long to wait for a message that should come, in tests that fail without it.
EXPECTED_WITHIN = 10


def restorer(buck: Buck, rel: str, content: str) -> Callable[[], None]:
    """Something that writes `content` back to `rel`."""
    return lambda: write(buck, rel, content)


async def broken_analysis_then_fixed(
    buck: Buck, target: str, breaks: Callable[[], Callable[[], None]]
) -> None:
    """Require that after `breaks` makes the analysis of `target` fail, and the function it
    returns puts that right, the server is told that the analysis works again with
    `graph_changed`."""
    dev = await start_dev(buck, target)
    try:
        first = await dev.start()
        fix = breaks()
        await dev.expect("error")
        await dev.wait_until_quiet()
        fix()
        await dev.expect("graph_changed", timeout=EXPECTED_WITHIN)
        assert_kept(dev, first)
    finally:
        await dev.stop()


def bogus_mode(buck: Buck, target: str, mode: str) -> Callable[[], None]:
    """Make the analysis of `target` fail with `fail()`."""
    return restorer(
        buck,
        APP,
        edit_target(buck, APP, target, f'mode = "{mode}"', 'mode = "bogus"'),
    )


def appended(buck: Buck, rel: str, text: str) -> Callable[[], None]:
    """Append `text` to `rel`."""
    content = read(buck, rel)
    write(buck, rel, content + text)
    return restorer(buck, rel, content)


def moved_away(buck: Buck, rel: str) -> Callable[[], None]:
    """Move the directory `rel` away."""
    os.rename(buck.cwd / rel, buck.cwd / (rel + "_moved"))
    return lambda: os.rename(buck.cwd / (rel + "_moved"), buck.cwd / rel)


class DevGraphTest(BuckTestCase):
    @dev_test()
    async def test_target_removed(self, buck: Buck) -> None:
        """The dev target removed from its build file: `error`, and the server keeps running.
        Once it is back, changes build."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            targets = remove_target(buck, APP, "all")
            (error,) = await dev.expect("error")
            assert "all" in error["error"], error["error"]
            await dev.quiet()
            assert_kept(dev, first)

            write(buck, APP, targets)
            await dev.wait_until_quiet()
            write(buck, "app/main.txt", "app v2\n")
            stale, _, built = await dev.expect("stale", "build", "built")
            assert files(buck, stale["outputs"]["site"]) == ["app/main.txt"]
            assert site(built) == "app v2\n"
            assert built["pid"] == first["pid"]
        finally:
            await dev.stop()

    @dev_test()
    async def test_target_restored_is_reported(self, buck: Buck) -> None:
        """The dev target removed and then restored: the server gets `error`, then
        `graph_changed` once the target is back."""
        await broken_analysis_then_fixed(
            buck,
            "//app:all",
            lambda: restorer(buck, APP, remove_target(buck, APP, "all")),
        )

    @dev_test()
    async def test_source_changed_while_target_removed(self, buck: Buck) -> None:
        """A source file that changes while the dev target is missing is built once the target is
        back, after `graph_changed`."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            targets = remove_target(buck, APP, "all")
            await dev.expect("error")

            write(buck, "app/main.txt", "app v2\n")
            await dev.wait_until_quiet()

            write(buck, APP, targets)
            _, stale, _, built = await dev.expect("graph_changed", "stale", "build", "built")
            assert files(buck, stale["outputs"]["site"]) == ["app/main.txt"]
            assert site(built) == "app v2\n"
            assert built["pid"] == first["pid"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_rule_stops_returning_dev_info(self, buck: Buck) -> None:
        """The rule stops returning `DevInfo`: `error`, and the server keeps running. Once it
        returns it again, changes build."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            targets = edit_target(
                buck,
                APP,
                "all",
                'mode = "outputs",',
                'mode = "outputs",\n    dev_info = False,',
            )
            (error,) = await dev.expect("error")
            assert "no `DevInfo` provider" in error["error"], error["error"]
            await dev.quiet()
            assert_kept(dev, first)

            write(buck, APP, targets)
            await dev.wait_until_quiet()
            write(buck, "app/main.txt", "app v2\n")
            _, _, built = await dev.expect("stale", "build", "built")
            assert built["pid"] == first["pid"]
        finally:
            await dev.stop()

    @dev_test()
    async def test_dev_info_restored_is_reported(self, buck: Buck) -> None:
        """The rule stops returning `DevInfo` and then returns it again: `error`, then
        `graph_changed`."""
        await broken_analysis_then_fixed(
            buck,
            "//app:all",
            lambda: restorer(
                buck,
                APP,
                edit_target(
                    buck,
                    APP,
                    "all",
                    'mode = "outputs",',
                    'mode = "outputs",\n    dev_info = False,',
                ),
            ),
        )

    @dev_test()
    async def test_syntax_error_in_build_file(self, buck: Buck) -> None:
        """A syntax error in the build file: `error`, and the server keeps running. Another
        syntax error in the broken file is another `error`."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            targets = read(buck, APP)
            write(buck, APP, targets + "\nthis is not starlark (\n")
            (error,) = await dev.expect("error")
            assert "TARGETS.fixture" in error["error"], error["error"]
            await dev.quiet()
            assert_kept(dev, first)

            write(buck, APP, targets + "\nthis is not starlark either (\n")
            await dev.expect("error")
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_syntax_error_in_build_file_fixed_is_reported(
        self, buck: Buck
    ) -> None:
        """A syntax error in the build file, then fixed: `error`, then `graph_changed`."""
        await broken_analysis_then_fixed(
            buck,
            "//app:all",
            lambda: appended(buck, APP, "\nthis is not starlark (\n"),
        )

    @dev_test()
    async def test_syntax_error_in_bzl_file(self, buck: Buck) -> None:
        """A syntax error in a `.bzl` file that the build file loads: `error`, and the server
        keeps running. Once fixed, changes build."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            defs = read(buck, "defs.bzl")
            write(buck, "defs.bzl", defs + "\ndef broken(:\n")
            (error,) = await dev.expect("error")
            assert "defs.bzl" in error["error"], error["error"]
            await dev.quiet()
            assert_kept(dev, first)

            write(buck, "defs.bzl", defs)
            await dev.wait_until_quiet()
            write(buck, "app/main.txt", "app v2\n")
            _, _, built = await dev.expect("stale", "build", "built")
            assert site(built) == "app v2\n"
            assert built["pid"] == first["pid"]
        finally:
            await dev.stop()

    @dev_test()
    async def test_syntax_error_in_bzl_file_fixed_is_reported(self, buck: Buck) -> None:
        """A syntax error in a `.bzl` file, then fixed: `error`, then `graph_changed`."""
        await broken_analysis_then_fixed(
            buck,
            "//app:all",
            lambda: appended(buck, "defs.bzl", "\ndef broken(:\n"),
        )

    @dev_test()
    async def test_bzl_change_makes_every_output_stale(self, buck: Buck) -> None:
        """A `.bzl` change that changes no output: `graph_changed`, and every output that the
        server asked for is stale, with the `.bzl` file in `files`. It builds unchanged."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            write(buck, "defs.bzl", read(buck, "defs.bzl") + "# changed\n")
            _, stale, build, built = await dev.expect("graph_changed", "stale", "build", "built")
            assert files(buck, stale["outputs"]["site"]) == ["defs.bzl"]
            assert build["outputs"] == ["site"]
            assert changed(built) == {"site": False}
            assert built["pid"] == first["pid"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_fail_in_analysis_of_the_target(self, buck: Buck) -> None:
        """`fail()` in the analysis of the dev target: `error`, and the server keeps running."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            edit_target(buck, APP, "all", 'mode = "outputs"', 'mode = "bogus"')
            (error,) = await dev.expect("error")
            assert "unknown mode bogus" in error["error"], error["error"]
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_fail_in_analysis_fixed_is_reported(self, buck: Buck) -> None:
        """`fail()` in the analysis of the dev target, then fixed: `error`, then
        `graph_changed`."""
        await broken_analysis_then_fixed(
            buck, "//app:all", lambda: bogus_mode(buck, "all", "outputs")
        )

    @dev_test()
    async def test_fail_in_analysis_of_a_dependency(self, buck: Buck) -> None:
        """`fail()` in the analysis of a dependency: `error`, and the server keeps running.
        A source change made while it fails is built once it is fixed, after `graph_changed`."""
        dev = await start_dev(buck, "//app:with_dep")
        try:
            first = await dev.start()
            assert site(first) == "lib v1\napp v1\n"
            lib = edit_target(
                buck,
                LIB,
                "lib",
                'srcs = ["lib.txt"],',
                'srcs = ["lib.txt"],\n    fail_analysis = True,',
            )
            (error,) = await dev.expect("error")
            assert "`fail_analysis` is set" in error["error"], error["error"]
            await dev.quiet()
            assert_kept(dev, first)

            write(buck, "lib/lib.txt", "lib v2\n")
            await dev.wait_until_quiet()

            write(buck, LIB, lib)
            _, stale, _, built = await dev.expect("graph_changed", "stale", "build", "built")
            assert files(buck, stale["outputs"]["site"]) == ["lib/lib.txt"]
            assert site(built) == "lib v2\napp v1\n"
            assert built["pid"] == first["pid"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_dependency_deleted(self, buck: Buck) -> None:
        """A dependency's target deleted from its build file: `error`, and the server keeps
        running."""
        dev = await start_dev(buck, "//app:with_dep")
        try:
            first = await dev.start()
            remove_target(buck, LIB, "lib")
            (error,) = await dev.expect("error")
            assert "lib" in error["error"], error["error"]
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_dependency_restored_is_reported(self, buck: Buck) -> None:
        """A dependency's target deleted and then restored: `error`, then `graph_changed`."""
        await broken_analysis_then_fixed(
            buck,
            "//app:with_dep",
            lambda: restorer(buck, LIB, remove_target(buck, LIB, "lib")),
        )

    @dev_test()
    async def test_dependency_renamed(self, buck: Buck) -> None:
        """A dependency renamed: `error` until the dev target uses the new name, then
        `graph_changed` and a build."""
        dev = await start_dev(buck, "//app:with_dep")
        try:
            first = await dev.start()
            edit_target(buck, LIB, "lib", 'name = "lib",', 'name = "lib2",')
            await dev.expect("error")
            await dev.quiet()
            assert_kept(dev, first)

            edit_target(buck, APP, "with_dep", '"//lib:lib"', '"//lib:lib2"')
            _, _, build, built = await dev.expect("graph_changed", "stale", "build", "built")
            assert build["outputs"] == ["site"]
            assert site(built) == "lib v1\napp v1\n"
            assert built["pid"] == first["pid"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_package_directory_removed(self, buck: Buck) -> None:
        """The package directory of a dependency moved away: `error`, and the server keeps
        running. Once it is back, changes build."""
        dev = await start_dev(buck, "//app:with_dep")
        try:
            first = await dev.start()
            os.rename(buck.cwd / "lib", buck.cwd / "lib_moved")
            await dev.expect("error")
            await dev.quiet()
            assert_kept(dev, first)

            os.rename(buck.cwd / "lib_moved", buck.cwd / "lib")
            await dev.wait_until_quiet()
            write(buck, "lib/lib.txt", "lib v2\n")
            _, _, built = await dev.expect("stale", "build", "built")
            assert site(built) == "lib v2\napp v1\n"
            assert built["pid"] == first["pid"]
        finally:
            await dev.stop()

    @dev_test()
    async def test_package_directory_restored_is_reported(self, buck: Buck) -> None:
        """The package directory of a dependency moved away and back: `error`, then
        `graph_changed`."""
        await broken_analysis_then_fixed(
            buck, "//app:with_dep", lambda: moved_away(buck, "lib")
        )

    @dev_test()
    async def test_dependency_added(self, buck: Buck) -> None:
        """A dependency added to the dev target: `graph_changed`, a build, and the new
        dependency's files are watched."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            edit_target(
                buck,
                APP,
                "all",
                'mode = "outputs",',
                'mode = "outputs",\n    deps = ["//lib:lib"],',
            )
            _, stale, _, built = await dev.expect("graph_changed", "stale", "build", "built")
            assert files(buck, stale["outputs"]["site"]) == [APP]
            assert site(built) == "lib v1\napp v1\n"
            assert built["pid"] == first["pid"]

            write(buck, "lib/lib.txt", "lib v2\n")
            stale, _, built = await dev.expect("stale", "build", "built")
            assert files(buck, stale["outputs"]["site"]) == ["lib/lib.txt"]
            assert site(built) == "lib v2\napp v1\n"
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_dependency_removed(self, buck: Buck) -> None:
        """A dependency removed from the dev target: `graph_changed`, a build, and its files are
        no longer watched."""
        dev = await start_dev(buck, "//app:with_dep")
        try:
            first = await dev.start()
            edit_target(buck, APP, "with_dep", '    deps = ["//lib:lib"],\n', "")
            _, _, _, built = await dev.expect("graph_changed", "stale", "build", "built")
            assert site(built) == "app v1\n"
            assert built["pid"] == first["pid"]

            write(buck, "lib/lib.txt", "lib v2\n")
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_server_command_line_changed(self, buck: Buck) -> None:
        """The server's command line changed: the old server is stopped, and the new one gets
        `ready`, asks for `site`, and gets it."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            edit_target(
                buck,
                APP,
                "all",
                'mode = "outputs",',
                'mode = "outputs",\n    server_args = ["--new"],',
            )
            shutdown, started = await dev.expect("shutdown", "started")
            assert shutdown["pid"] == first["pid"]
            assert started["args"] == ["--new"]
            rest = await dev.collect()
            assert rest[0]["type"] == "ready", summary(rest)
            build, built = rest[-2:]
            assert (build["type"], built["type"]) == ("build", "built"), summary(rest)
            assert build["outputs"] == ["site"]
            assert all(e["pid"] == started["pid"] for e in rest)
            assert site(built) == "app v1\n"
        finally:
            await dev.stop()

    @dev_test()
    async def test_new_server_gets_no_answer_it_did_not_ask_for(self, buck: Buck) -> None:
        """The server's command line changed, and the build that the change asks for is still
        running when the new server connects: the new server does not hear of that build. It
        gets one answer for each output that it asks for, and no other."""
        write(buck, "app/delay.txt", "2\n")
        dev = await start_dev(buck, "//app:slow")
        try:
            first = await dev.start()
            # The output changes too, so that its build takes the delay.
            edit_target(
                buck,
                APP,
                "slow",
                'srcs = ["main.txt"],',
                'srcs = ["main.txt", "other.txt"],\n    server_args = ["--new"],',
            )
            _, started = await dev.expect("shutdown", "started")
            rest = await dev.collect()
            assert all(e["pid"] == started["pid"] for e in rest)
            # The outputs that the new server asked for and has no answer for yet. One answer
            # covers two requests for an output that is still building.
            asked: Set[str] = set()
            for e in rest:
                if e["type"] == "build":
                    asked |= set(e["outputs"])
                if e["type"] in ("built", "failed", "cancelled"):
                    assert e["output"] in asked, (
                        f"the new server got `{e['type']}` for `{e['output']}`, which it did "
                        f"not ask for: {summary(rest)}"
                    )
                    asked.remove(e["output"])
            assert asked == set(), f"no answer for {asked}: {summary(rest)}"
            assert first["pid"] != started["pid"]
        finally:
            await dev.stop()

    @dev_test()
    async def test_output_added(self, buck: Buck) -> None:
        """An output added to `DevInfo`: `graph_changed`, and the output that the server has is
        stale. The new one is not: the server never asked for it. It can ask for it now."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            edit_target(
                buck,
                APP,
                "all",
                'mode = "outputs",',
                'mode = "outputs",\n    extra_outputs = {"extra": ["extra.txt"]},',
            )
            _, stale, build, built = await dev.expect(
                "graph_changed", "stale", "build", "built"
            )
            assert list(stale["outputs"]) == ["site"]
            assert build["outputs"] == ["site"]
            assert changed(built) == {"site": False}
            await dev.quiet()

            dev.request({"type": "build", "outputs": ["extra"]})
            build, built = await dev.expect("build", "built")
            assert build["outputs"] == ["extra"]
            assert contents(built) == {"extra": "extra v1\n"}
            assert built["pid"] == first["pid"]
        finally:
            await dev.stop()

    @dev_test()
    async def test_output_renamed(self, buck: Buck) -> None:
        """The output that the server uses renamed: the server never asked for the new name, so
        nothing is stale. It can ask for the new name, and its request for the old name fails.
        The server keeps running."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            edit_target(
                buck,
                APP,
                "all",
                'mode = "outputs",',
                'mode = "outputs",\n    site_name = "page",',
            )
            await dev.expect("graph_changed")
            await dev.quiet()

            dev.request({"type": "build", "outputs": ["page"]})
            build, built = await dev.expect("build", "built")
            assert build["outputs"] == ["page"]
            assert contents(built) == {"page": "app v1\n"}
            # The digest is that of `site`, but the name is new.
            assert changed(built) == {"page": True}

            dev.request({"type": "build", "outputs": ["site"]})
            build, failed = await dev.expect("build", "failed")
            assert build["outputs"] == ["site"]
            assert failed["output"] == "site"
            assert "no output named `site`" in failed["error"], failed["error"]
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_output_removed(self, buck: Buck) -> None:
        """An output removed: `graph_changed`, the rest builds, and the server's request for the
        removed output fails. The other output of that request builds."""
        dev = await start_dev(buck, "//app:multi")
        try:
            first = await dev.start(outputs=2)
            assert contents(first) == {"site": "app v1\n", "extra": "extra v1\n"}
            edit_target(
                buck,
                APP,
                "multi",
                '    extra_outputs = {"extra": ["extra.txt"]},\n',
                "",
            )
            _, _, build, built = await dev.expect("graph_changed", "stale", "build", "built")
            assert build["outputs"] == ["site"]
            assert changed(built) == {"site": False}

            dev.request({"type": "build", "outputs": ["site", "extra"]})
            await dev.expect("build")
            results = await dev.results(2)
            assert results["site"]["type"] == "built", summary(list(results.values()))
            assert changed(results["site"]) == {"site": False}
            assert results["extra"]["type"] == "failed", summary(list(results.values()))
            error = results["extra"]["error"]
            assert "no output named `extra`" in error, error
            await dev.quiet()
            assert_kept(dev, first["site"])
        finally:
            await dev.stop()

    @dev_test()
    async def test_comment_in_build_file(self, buck: Buck) -> None:
        """A comment added to the build file changes no target: `graph_changed`, and nothing is
        built."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            write(buck, APP, read(buck, APP) + "# a comment\n")
            await dev.expect("graph_changed")
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_another_target_in_the_package_broken(self, buck: Buck) -> None:
        """A target outside the graph, in the dev target's package, broken: the package does not
        load, so `error`, and the server keeps running."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            edit_target(
                buck, APP, "other", 'srcs = ["other.txt"]', 'srcs = ["missing.txt"]'
            )
            (error,) = await dev.expect("error")
            assert "missing.txt" in error["error"], error["error"]
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_another_target_in_the_package_fixed_is_reported(
        self, buck: Buck
    ) -> None:
        """A target outside the graph, in the dev target's package, broken and then fixed:
        `error`, then `graph_changed`."""
        await broken_analysis_then_fixed(
            buck,
            "//app:all",
            lambda: restorer(
                buck,
                APP,
                edit_target(
                    buck, APP, "other", 'srcs = ["other.txt"]', 'srcs = ["missing.txt"]'
                ),
            ),
        )

    @dev_test()
    async def test_another_target_in_the_package_changed(self, buck: Buck) -> None:
        """A target outside the graph, in the dev target's package, changed: `graph_changed`,
        and nothing is built."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            edit_target(
                buck, APP, "other", 'srcs = ["other.txt"]', 'srcs = ["main.txt"]'
            )
            await dev.expect("graph_changed")
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_unrelated_change_while_analysis_broken_sends_nothing(
        self, buck: Buck
    ) -> None:
        """While the analysis fails, a change to a file that nothing uses sends nothing, as it
        does while the analysis works."""
        dev = await start_dev(buck, "//app:all")
        try:
            first = await dev.start()
            edit_target(buck, APP, "all", 'mode = "outputs"', 'mode = "bogus"')
            await dev.expect("error")
            await dev.quiet()

            write(buck, "app/unrelated.txt", "unrelated\n")
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()

    @dev_test()
    async def test_server_that_logs_to_the_project_gets_one_error(
        self, buck: Buck
    ) -> None:
        """A server that writes a file in the project for each message gets one `error` when
        the analysis breaks, not one more for each that it logs."""
        dev = await start_dev(buck, "//app:project_log")
        try:
            first = await dev.start()
            # A working analysis: logging `built` changes nothing.
            await dev.quiet(2)
            edit_target(buck, APP, "project_log", 'mode = "outputs"', 'mode = "bogus"')
            await dev.expect("error")
            await dev.quiet()
            assert_kept(dev, first)
        finally:
            await dev.stop()
