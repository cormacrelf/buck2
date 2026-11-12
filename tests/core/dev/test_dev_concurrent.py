# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# pyre-strict

# Builds that the server asks for while other builds run. They use the fixture of `test_dev`.
# `//app:outputs_slow` has `site`, which waits for the number of seconds in `app/delay.txt`, and
# `extra`, which does not wait.

import asyncio
from typing import Any, Dict, Tuple

from buck2.tests.core.common.dev.dev_util import (
    changed,
    contents,
    DevSession,
    dev_test,
    files,
    start_dev,
    write,
)
from buck2.tests.e2e_util.api.buck import Buck
from buck2.tests.e2e_util.buck_workspace import BuckTestCase


async def start_slow(buck: Buck, delay: int) -> Tuple[DevSession, Dict[str, Any]]:
    """Start `buck2 dev` on `//app:outputs_slow` with a server that does not answer `stale`,
    then make `site` take `delay` seconds to build. Returns the `stale` that follows."""
    dev = await start_dev(buck, "//app:outputs_slow", "none", "--", "--manual")
    await dev.start()
    write(buck, "app/delay.txt", f"{delay}\n")
    (stale,) = await dev.expect("stale")
    return dev, stale


class DevConcurrentTest(BuckTestCase):
    @dev_test()
    async def test_fast_output_during_a_slow_build(self, buck: Buck) -> None:
        """A build that the server asks for during a slow one starts at once, and ends before
        the slow one."""
        dev, _ = await start_slow(buck, 8)
        try:
            dev.request({"type": "build", "outputs": ["site"]})
            (slow,) = await dev.expect("building")
            dev.request({"type": "build", "outputs": ["extra"]})
            fast, fast_built, slow_built = await dev.expect(
                "building", "built", "built"
            )
            assert fast["gen"] == slow["gen"] + 1
            assert fast_built["gen"] == fast["gen"]
            assert contents(fast_built) == {"extra": "extra v1\n"}
            assert fast_built["last_of_generation"]
            assert slow_built["gen"] == slow["gen"]
            assert contents(slow_built) == {"site": "lib v1\napp v1\n"}
            assert slow_built["last_of_generation"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_output_that_a_build_is_building_is_joined(self, buck: Buck) -> None:
        """An output that a build that runs is building is not built again: the new build gets
        it when the first one has it. The new build's own outputs come before that."""
        dev, _ = await start_slow(buck, 6)
        try:
            write(buck, "app/main.txt", "app v2\n")
            await dev.expect("stale")
            dev.request({"type": "build", "outputs": ["site"]})
            (first,) = await dev.expect("building")
            dev.request(
                {"type": "build", "outputs": ["extra", "site"], "streaming": True}
            )
            second, extra, site, joined = await dev.expect(
                "building", "built", "built", "built"
            )
            assert second["outputs"] == ["extra", "site"]
            assert extra["gen"] == second["gen"]
            assert contents(extra) == {"extra": "extra v1\n"}
            assert not extra["last_of_generation"]
            assert site["gen"] == first["gen"]
            assert changed(site) == {"site": True}
            assert joined["gen"] == second["gen"]
            assert joined["last_of_generation"]
            assert contents(joined) == {"site": "lib v1\napp v2\n"}
            assert joined["outputs"]["site"]["path"] == site["outputs"]["site"]["path"]
            assert changed(joined) == {"site": False}
            await dev.quiet()

            # Both builds cleared the changes before them.
            write(buck, "app/main.txt", "app v3\n")
            (stale,) = await dev.expect("stale")
            assert {
                name: files(buck, entry) for name, entry in stale["outputs"].items()
            } == {"site": ["app/main.txt"]}
        finally:
            await dev.stop()

    @dev_test()
    async def test_change_cancels_the_builds_that_wait_for_its_outputs(
        self, buck: Buck
    ) -> None:
        """A change stops the build of `site`, and the build that joined it for `site` is
        cancelled too. Both builds' outputs are stale."""
        dev, _ = await start_slow(buck, 60)
        try:
            dev.request({"type": "build", "outputs": ["site"]})
            (first,) = await dev.expect("building")
            dev.request({"type": "build", "outputs": ["site"], "streaming": True})
            (second,) = await dev.expect("building")
            await asyncio.sleep(2)

            write(buck, "app/delay.txt", "0\n")
            write(buck, "app/main.txt", "app v2\n")
            cancelled = await dev.expect("cancelled", "cancelled")
            assert sorted(c["gen"] for c in cancelled) == [first["gen"], second["gen"]]
            (stale,) = await dev.expect("stale")
            assert list(stale["outputs"]) == ["site"]

            dev.request({"type": "build", "outputs": ["site"]})
            _, built = await dev.expect("building", "built")
            assert contents(built) == {"site": "lib v1\napp v2\n"}
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_build_after_a_change_waits_for_the_builds_that_run(
        self, buck: Buck
    ) -> None:
        """Once a file changed, a build that the server asks for waits until the builds that run
        are done, and uses the change."""
        dev, _ = await start_slow(buck, 4)
        try:
            dev.request({"type": "build", "outputs": ["site"]})
            (slow,) = await dev.expect("building")
            # `site` is not built from `extra.txt`, so this does not stop its build.
            write(buck, "app/extra.txt", "extra v2\n")
            await asyncio.sleep(1)
            dev.request({"type": "build", "outputs": ["extra"]})
            slow_built, fast, fast_built = await dev.expect(
                "built", "building", "built"
            )
            assert slow_built["gen"] == slow["gen"]
            assert fast["gen"] == slow["gen"] + 1
            assert contents(fast_built) == {"extra": "extra v2\n"}
            # The build of `extra` had the change, so it is not stale.
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_failure_of_a_build_next_to_another(self, buck: Buck) -> None:
        """A build that fails while another runs: it ends in `failed`, and `buck2 dev` prints
        the error, which the console does not show for it."""
        dev = await start_dev(buck, "//app:outputs_slow", "none", "--", "--manual")
        await dev.start()
        try:
            write(buck, "app/delay.txt", "6\n")
            write(buck, "app/extra.txt", "FAIL extra\n")
            await dev.expect("stale")
            dev.request({"type": "build", "outputs": ["site"]})
            (slow,) = await dev.expect("building")
            dev.request({"type": "build", "outputs": ["extra"]})
            fast, failed, slow_built = await dev.expect("building", "failed", "built")
            assert failed["gen"] == fast["gen"]
            assert "an input says FAIL" in failed["error"], failed
            assert slow_built["gen"] == slow["gen"]
            await dev.quiet()
        finally:
            output = await dev.stop()
        assert "could not build `extra`" in output, output

    @dev_test()
    async def test_forget_during_a_build(self, buck: Buck) -> None:
        """`forget` during the build of the output: the server still gets the output, but it is
        not tracked any more."""
        dev, _ = await start_slow(buck, 4)
        try:
            dev.request({"type": "build", "outputs": ["extra", "site"]})
            (building,) = await dev.expect("building")
            dev.request({"type": "forget", "outputs": ["site"]})
            (built,) = await dev.expect("built")
            assert built["gen"] == building["gen"]
            assert sorted(built["outputs"]) == ["extra", "site"]

            write(buck, "app/main.txt", "app v2\n")
            await dev.quiet()
            write(buck, "app/extra.txt", "extra v2\n")
            (stale,) = await dev.expect("stale")
            assert list(stale["outputs"]) == ["extra"]
        finally:
            await dev.stop()
