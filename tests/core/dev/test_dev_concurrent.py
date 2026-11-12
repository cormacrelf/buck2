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
            await dev.expect("build")
            dev.request({"type": "build", "outputs": ["extra"]})
            _, fast, slow = await dev.expect("build", "built", "built")
            assert contents(fast) == {"extra": "extra v1\n"}
            assert contents(slow) == {"site": "lib v1\napp v1\n"}
            # The second build started after the first.
            assert fast["token"] > slow["token"]
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_output_that_a_build_is_building_is_joined(self, buck: Buck) -> None:
        """An output that is building is not built again: a `build` of it gets the result of
        the build that runs, once. The other outputs of that `build` come before."""
        dev, _ = await start_slow(buck, 6)
        try:
            write(buck, "app/main.txt", "app v2\n")
            await dev.expect("stale")
            dev.request({"type": "build", "outputs": ["site"]})
            await dev.expect("build")
            dev.request({"type": "build", "outputs": ["extra", "site"]})
            _, extra, site = await dev.expect("build", "built", "built")
            assert contents(extra) == {"extra": "extra v1\n"}
            assert contents(site) == {"site": "lib v1\napp v2\n"}
            assert site["changed"]
            assert site["token"] < extra["token"]
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
    async def test_change_cancels_the_build_of_an_output(self, buck: Buck) -> None:
        """A change stops the build of `site`, which the server asked for twice: it gets one
        `cancelled`, and `site` is stale."""
        dev, _ = await start_slow(buck, 60)
        try:
            dev.request({"type": "build", "outputs": ["site"]})
            dev.request({"type": "build", "outputs": ["site"]})
            await dev.expect("build", "build")
            await asyncio.sleep(2)

            write(buck, "app/delay.txt", "0\n")
            write(buck, "app/main.txt", "app v2\n")
            cancelled, stale = await dev.expect("cancelled", "stale")
            assert cancelled["output"] == "site"
            assert list(stale["outputs"]) == ["site"]

            dev.request({"type": "build", "outputs": ["site"]})
            _, built = await dev.expect("build", "built")
            assert contents(built) == {"site": "lib v1\napp v2\n"}
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_change_stops_only_the_outputs_built_from_it(self, buck: Buck) -> None:
        """A change to a file that `site` is built from stops its build, but not the build of
        `extra` that the same request asked for."""
        dev, _ = await start_slow(buck, 60)
        try:
            dev.request({"type": "build", "outputs": ["extra", "site"]})
            _, extra = await dev.expect("build", "built")
            assert contents(extra) == {"extra": "extra v1\n"}

            write(buck, "app/delay.txt", "0\n")
            cancelled, stale = await dev.expect("cancelled", "stale")
            assert cancelled["output"] == "site"
            assert list(stale["outputs"]) == ["site"]
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
            await dev.expect("build")
            # `site` is not built from `extra.txt`, so this does not stop its build.
            write(buck, "app/extra.txt", "extra v2\n")
            await asyncio.sleep(1)
            dev.request({"type": "build", "outputs": ["extra"]})
            _, slow, fast = await dev.expect("build", "built", "built")
            assert slow["output"] == "site"
            assert contents(fast) == {"extra": "extra v2\n"}
            assert fast["token"] > slow["token"]
            # The build of `extra` had the change, so it is not stale.
            await dev.quiet()
        finally:
            await dev.stop()

    @dev_test()
    async def test_failure_of_a_build_next_to_another(self, buck: Buck) -> None:
        """An output that fails while another builds: it gets `failed`, and `buck2 dev` prints
        the error, which the console does not show."""
        dev = await start_dev(buck, "//app:outputs_slow", "none", "--", "--manual")
        await dev.start()
        try:
            write(buck, "app/delay.txt", "6\n")
            write(buck, "app/extra.txt", "FAIL extra\n")
            await dev.expect("stale")
            dev.request({"type": "build", "outputs": ["site"]})
            await dev.expect("build")
            dev.request({"type": "build", "outputs": ["extra"]})
            _, failed, slow = await dev.expect("build", "failed", "built")
            assert failed["output"] == "extra"
            assert "an input says FAIL" in failed["error"], failed
            assert slow["output"] == "site"
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
            await dev.expect("build")
            dev.request({"type": "forget", "outputs": ["site"]})
            results = await dev.results(2)
            assert sorted(results) == ["extra", "site"]
            assert all(r["type"] == "built" for r in results.values()), results

            write(buck, "app/main.txt", "app v2\n")
            await dev.quiet()
            write(buck, "app/extra.txt", "extra v2\n")
            (stale,) = await dev.expect("stale")
            assert list(stale["outputs"]) == ["extra"]
        finally:
            await dev.stop()
