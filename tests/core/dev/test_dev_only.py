# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# pyre-strict

# A prototype of the `only` proposal in `dev_review.md`: a `DevInfo` without `watch`, whose
# outputs carry the policy. `buck2 dev` finds what the server's command and each output may be
# built from. A file may belong to several of them, and an output with `only` is held back
# after a change outside its `only` until the server asks for it. They use the fixture of
# `test_dev`:
#
# - `//app:outputs` has `site`, built from `app/main.txt` and `//lib:lib`, and `extra`, built
#   from `app/extra.txt`.
# - `//app:only` has `site`, `exe` and `patch`, each built from `app/main.txt` and `//lib:lib`.
#   `exe` has `only = []` and `patch` has `only = [the target]`.
# - `//app:sidecars` has `site`, built from `app/main.txt`, with three sidecars: `extra` and
#   `check`, built from `app/extra.txt` and `app/check.txt`, and `src`, which is
#   `app/main.txt` itself.

import asyncio
import os
from typing import Any, Dict, Tuple

from buck2.tests.core.common.dev.dev_util import (
    APP,
    changed,
    contents,
    dev_test,
    edit_target,
    files,
    LIB,
    read,
    start_dev,
    write,
)
from buck2.tests.e2e_util.api.buck import Buck
from buck2.tests.e2e_util.buck_workspace import BuckTestCase


def sidecars(output: Dict[str, Any]) -> Dict[str, Tuple[str, bool]]:
    """The content of each sidecar of a built output, and whether it changed."""
    return {
        name: (sidecar["content"], sidecar["changed"])
        for name, sidecar in output["sidecars"].items()
    }


class DevOnlyTest(BuckTestCase):
    @dev_test()
    async def test_output_is_built_after_a_change_that_it_may_be_built_from(
        self, buck: Buck
    ) -> None:
        dev = await start_dev(buck, "//app:outputs")
        try:
            _, ready, _, built = await dev.expect(
                "started", "ready", "building", "built"
            )
            assert ready["outputs"] == ["site", "extra"]
            assert contents(built) == {
                "site": "lib v1\napp v1\n",
                "extra": "extra v1\n",
            }

            # Each file builds the outputs that may be built from it, and no other.
            write(buck, "app/main.txt", "app v2\n")
            building, built = await dev.expect("building", "built")
            assert building["outputs"] == ["site"]
            assert files(buck, building) == ["app/main.txt"]
            assert contents(built) == {"site": "lib v1\napp v2\n"}

            write(buck, "app/extra.txt", "extra v2\n")
            building, built = await dev.expect("building", "built")
            assert building["outputs"] == ["extra"]
            assert contents(built) == {"extra": "extra v2\n"}

            # A file of a dependency.
            write(buck, "lib/lib.txt", "lib v2\n")
            building, built = await dev.expect("building", "built")
            assert building["outputs"] == ["site"]
            assert files(buck, building) == ["lib/lib.txt"]
            assert contents(built) == {"site": "lib v2\napp v2\n"}

            # Both at once.
            write(buck, "app/main.txt", "app v3\n")
            write(buck, "app/extra.txt", "extra v3\n")
            building, built = await dev.expect("building", "built")
            assert building["outputs"] == ["extra", "site"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_server_file_is_only_checked(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:outputs")
        try:
            first = await dev.start()

            # No output is built from the server script, so a save of it builds nothing, and
            # the server's command is the same. So is a file that nothing is built from.
            os.utime(buck.cwd / "app/server.py")
            write(buck, "app/unrelated.txt", "unrelated\n")
            write(buck, "app/other.txt", "other v2\n")
            await dev.quiet()

            # New contents start the server again, and the new server asks for what it needs.
            write(buck, "app/server.py", read(buck, "app/server.py") + "# changed\n")
            shutdown, started, _, building, built = await dev.expect(
                "shutdown", "started", "ready", "building", "built"
            )
            assert shutdown["pid"] == first["pid"]
            assert built["pid"] == started["pid"]
            assert building["outputs"] == ["extra", "site"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_only_builds_after_a_change_to_its_targets(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:only")
        try:
            _, ready, _, built = await dev.expect(
                "started", "ready", "building", "built"
            )
            assert ready["outputs"] == ["site", "exe", "patch"]
            assert sorted(built["outputs"]) == ["exe", "patch", "site"]

            # The target's own file is in the `only` of `patch`, which is built with `site`.
            # It is outside the `only` of `exe`, which is empty: `exe` is stale from now on.
            write(buck, "app/main.txt", "app v2\n")
            stale, building, built = await dev.expect("stale", "building", "built")
            assert stale["outputs"] == ["exe"]
            assert files(buck, stale) == ["app/main.txt"]
            assert building["outputs"] == ["patch", "site"]
            assert files(buck, building) == ["app/main.txt"]
            assert contents(built) == {
                "patch": "lib v1\napp v2\n",
                "site": "lib v1\napp v2\n",
            }

            # Again: `exe` is stale already, and the server is not told twice.
            write(buck, "app/main.txt", "app v3\n")
            building, built = await dev.expect("building", "built")
            assert building["outputs"] == ["patch", "site"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_only_holds_one_output_while_others_build_from_the_same_file(
        self, buck: Buck
    ) -> None:
        dev = await start_dev(buck, "//app:only")
        try:
            await dev.start()

            # All three outputs are built from `lib`. It is outside the `only` of `patch` and
            # of `exe`, which are held back, and `site` is built with the change.
            write(buck, "lib/lib.txt", "lib v2\n")
            stale, building, built = await dev.expect("stale", "building", "built")
            assert stale["outputs"] == ["exe", "patch"]
            assert files(buck, stale) == ["lib/lib.txt"]
            assert building["outputs"] == ["site"]
            assert contents(built) == {"site": "lib v2\napp v1\n"}

            # While `patch` is held back, a change inside its `only` does not build it either:
            # it would be built against the new `lib`.
            write(buck, "app/main.txt", "app v2\n")
            stale, building, built = await dev.expect("stale", "building", "built")
            assert stale["outputs"] == ["exe", "patch"]
            assert files(buck, stale) == ["app/main.txt", "lib/lib.txt"]
            assert building["outputs"] == ["site"]
            assert contents(built) == {"site": "lib v2\napp v2\n"}

            # The server asks for `exe`, which lets it go. `patch` is still held back.
            dev.request({"type": "build", "outputs": ["exe"]})
            building, built, stale = await dev.expect("building", "built", "stale")
            assert contents(built) == {"exe": "lib v2\napp v2\n"}
            assert stale["outputs"] == ["patch"]
            assert files(buck, stale) == ["lib/lib.txt"]

            write(buck, "app/main.txt", "app v3\n")
            stale, building, built = await dev.expect("stale", "building", "built")
            assert stale["outputs"] == ["exe", "patch"]
            assert building["outputs"] == ["site"]

            # The server asks for `patch`, and from then on it is built by itself again.
            dev.request({"type": "build", "outputs": ["patch"]})
            building, built, stale = await dev.expect("building", "built", "stale")
            assert contents(built) == {"patch": "lib v2\napp v3\n"}
            assert stale["outputs"] == ["exe"]

            write(buck, "app/main.txt", "app v4\n")
            building, built = await dev.expect("building", "built")
            assert building["outputs"] == ["patch", "site"]
            assert contents(built)["patch"] == "lib v2\napp v4\n"
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_new_file_of_a_target_in_only_builds(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:only_pages")
        try:
            await dev.start()

            # The target's glob takes the new file in, so it is inside the `only` of `patch`.
            write(buck, "app/pages/b.txt", "page b\n")
            stale, _, building, built = await dev.expect(
                "stale", "graph_changed", "building", "built"
            )
            assert stale["outputs"] == ["exe"]
            assert building["outputs"] == ["patch", "site"]
            assert contents(built)["patch"] == "lib v1\npage a\npage b\n"

            # And it stays there.
            write(buck, "app/pages/b.txt", "page b2\n")
            building, built = await dev.expect("building", "built")
            assert building["outputs"] == ["patch", "site"]
            assert contents(built)["patch"] == "lib v1\npage a\npage b2\n"
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_changed_target_outside_only_holds(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:only")
        try:
            await dev.start()

            # A file that no target has yet.
            write(buck, "lib/more.txt", "more v1\n")
            await dev.quiet()

            # `lib` changes: it is one of the targets that `patch` may be built from, and is
            # outside its `only`.
            edit_target(buck, LIB, "lib", '"lib.txt"', '"lib.txt", "more.txt"')
            stale, _, building, built = await dev.expect(
                "stale", "graph_changed", "building", "built"
            )
            assert stale["outputs"] == ["exe", "patch"]
            assert files(buck, stale) == [LIB]
            assert building["outputs"] == ["site"]
            assert contents(built) == {"site": "lib v1\nmore v1\napp v1\n"}
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_bzl_change_holds_the_outputs_with_only(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:only")
        try:
            first = await dev.start()

            # A `.bzl` file may change what any rule does: the outputs with `only` are held
            # back, the others are built, and the server's command is built again.
            write(buck, "defs.bzl", read(buck, "defs.bzl") + "# changed\n")
            stale, _, building, built = await dev.expect(
                "stale", "graph_changed", "building", "built"
            )
            assert stale["outputs"] == ["exe", "patch"]
            assert files(buck, stale) == ["defs.bzl"]
            assert building["outputs"] == ["site"]
            assert changed(built) == {"site": False}
            assert built["pid"] == first["pid"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_new_server_has_nothing_stale(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:only")
        try:
            await dev.start()

            write(buck, "lib/lib.txt", "lib v2\n")
            stale, _, _ = await dev.expect("stale", "building", "built")
            assert stale["outputs"] == ["exe", "patch"]

            # A new server has no output, so none of its outputs is stale: it asks for what
            # it needs, and gets it as the files are.
            write(buck, "app/server.py", read(buck, "app/server.py") + "# changed\n")
            _, _, _, building, built = await dev.expect(
                "shutdown", "started", "ready", "building", "built"
            )
            assert building["outputs"] == ["exe", "patch", "site"]
            assert contents(built)["patch"] == "lib v2\napp v1\n"
            await dev.quiet()

            # `patch` is built by itself again.
            write(buck, "app/main.txt", "app v2\n")
            stale, building, _ = await dev.expect("stale", "building", "built")
            assert stale["outputs"] == ["exe"]
            assert building["outputs"] == ["patch", "site"]
        finally:
            await dev.stop()

    @dev_test()
    async def test_change_that_holds_an_output_stops_a_build_of_it(
        self, buck: Buck
    ) -> None:
        dev = await start_dev(buck, "//app:only_slow")
        try:
            await dev.start()

            # A build of `patch` and `site` that takes 60 seconds.
            write(buck, "app/delay.txt", "60\n")
            stale, first = await dev.expect("stale", "building")
            assert stale["outputs"] == ["exe"]
            assert first["outputs"] == ["patch", "site"]
            await asyncio.sleep(2)

            # `lib` changes while it runs. The build would use the change, so it stops, and
            # `patch` is not built again: it is held back now.
            write(buck, "app/delay.txt", "0\n")
            write(buck, "lib/lib.txt", "lib v2\n")
            cancelled, stale, building, built = await dev.expect(
                "cancelled", "stale", "building", "built", timeout=30
            )
            assert cancelled["gen"] == first["gen"]
            assert stale["outputs"] == ["exe", "patch"]
            assert building["outputs"] == ["site"]
            assert contents(built) == {"site": "lib v2\napp v1\n"}
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_build_that_the_server_asked_for_is_built_again_when_stopped(
        self, buck: Buck
    ) -> None:
        dev = await start_dev(buck, "//app:only_slow")
        try:
            await dev.start()

            write(buck, "lib/lib.txt", "lib v2\n")
            stale, _, _ = await dev.expect("stale", "building", "built")
            assert stale["outputs"] == ["exe", "patch"]

            # Make builds slow. `site` is built with the change, in 60 seconds, unless
            # something stops it.
            write(buck, "app/delay.txt", "60\n")
            await dev.expect("stale", "building")
            await asyncio.sleep(2)

            # The server asks for `exe`, which waits for the build of `site`. A change to
            # the target's own file stops that build, and the next one has both.
            dev.request({"type": "build", "outputs": ["exe"]})
            await asyncio.sleep(1)
            write(buck, "app/delay.txt", "0\n")
            write(buck, "app/main.txt", "app v2\n")
            await dev.expect("cancelled", timeout=30)
            got = await dev.collect()
            built = [e for e in got if e["type"] == "built"]
            outputs = {}
            for e in built:
                outputs.update(contents(e))
            # `exe` is built, with every change, and is not stale after it. `patch` is
            # still held back, and was not built.
            assert outputs == {
                "exe": "lib v2\napp v2\n",
                "site": "lib v2\napp v2\n",
            }, got
            stale = [e for e in got if e["type"] == "stale"]
            assert stale[-1]["outputs"] == ["patch"], got
        finally:
            await dev.stop()

    @dev_test()
    async def test_sidecars_are_built_with_their_output(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:sidecars")
        try:
            _, ready, _, built = await dev.expect(
                "started", "ready", "building", "built"
            )
            # A sidecar is not an output.
            assert ready["outputs"] == ["site"]
            check = read(buck, "app/check.txt")
            assert sidecars(built["outputs"]["site"]) == {
                "check": (check, True),
                "extra": ("extra v1\n", True),
                "src": ("app v1\n", True),
            }

            # A file that only a sidecar is built from builds the output. The sidecar is new,
            # and the output and the other sidecars are not.
            write(buck, "app/extra.txt", "extra v2\n")
            building, built = await dev.expect("building", "built")
            assert building["outputs"] == ["site"]
            assert files(buck, building) == ["app/extra.txt"]
            assert changed(built) == {"site": False}
            assert sidecars(built["outputs"]["site"]) == {
                "check": (check, False),
                "extra": ("extra v2\n", True),
                "src": ("app v1\n", False),
            }

            # A file of the output, which is a sidecar itself.
            write(buck, "app/main.txt", "app v2\n")
            _, built = await dev.expect("building", "built")
            assert contents(built) == {"site": "app v2\n"}
            assert changed(built) == {"site": True}
            assert sidecars(built["outputs"]["site"]) == {
                "check": (check, False),
                "extra": ("extra v2\n", False),
                "src": ("app v2\n", True),
            }

            # The server cannot ask for a sidecar by itself.
            dev.request({"type": "build", "outputs": ["extra"]})
            _, failed = await dev.expect("building", "failed")
            assert "no output named `extra`" in failed["error"], failed
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_writes_under_buck_out_do_not_start_builds(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//app:flood")
        try:
            # The action of `flood` writes about a hundred thousand files under `buck-out`.
            # They are no changes to the project: the build is not stopped, and nothing is
            # built after it.
            _, _, _, built = await dev.expect("started", "ready", "building", "built")
            assert contents(built) == {"flood": "app v1\n", "site": "app v1\n"}
            await dev.quiet(5)

            write(buck, "app/main.txt", "app v2\n")
            building, built = await dev.expect("building", "built")
            assert files(buck, building) == ["app/main.txt"]
            assert contents(built) == {"flood": "app v2\n", "site": "app v2\n"}
            await dev.quiet(5)
        finally:
            await dev.stop()

    @dev_test()
    async def test_directory_made_in_the_project_root_is_watched(
        self, buck: Buck
    ) -> None:
        dev = await start_dev(buck, "//app:outputs")
        try:
            await dev.start()

            # A package in a directory that the root of the project gets after the daemon
            # started watching, and that `site` is then built from.
            write(buck, "more/more.txt", "more v1\n")
            write(
                buck,
                "more/TARGETS.fixture",
                'load("//:defs.bzl", "concat")\n\n'
                'concat(name = "more", srcs = ["more.txt"], visibility = ["PUBLIC"])\n',
            )
            await dev.quiet()
            edit_target(buck, APP, "outputs", '"//lib:lib"', '"//lib:lib", "//more:more"')
            # The target of both outputs changed, so both are built.
            _, building, built = await dev.expect("graph_changed", "building", "built")
            assert building["outputs"] == ["extra", "site"]
            assert contents(built)["site"] == "lib v1\nmore v1\napp v1\n"

            # A change to a file in that directory is seen.
            write(buck, "more/more.txt", "more v2\n")
            building, built = await dev.expect("building", "built")
            assert files(buck, building) == ["more/more.txt"]
            assert contents(built) == {"site": "lib v1\nmore v2\napp v1\n"}
            await dev.quiet()
        finally:
            await dev.stop()
