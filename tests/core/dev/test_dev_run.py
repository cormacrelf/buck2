# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# pyre-strict

# Tests for what `buck2 dev` runs: arguments after `--`, the environment of the client, and a
# target with only a `RunInfo`. The targets are in `run/` of the fixture; their server
# (`run/server.py`) does not connect, and logs `started` with its arguments and environment.

from buck2.tests.core.common.dev.dev_util import (
    alive,
    dev_test,
    read,
    start_dev,
    wait_until,
    write,
)
from buck2.tests.e2e_util.api.buck import Buck
from buck2.tests.e2e_util.asserts import expect_failure
from buck2.tests.e2e_util.buck_workspace import BuckTestCase


SERVER = "run/server.py"


def edit_server(buck: Buck) -> None:
    """Give the server script new contents, which do not change what it does."""
    write(buck, SERVER, read(buck, SERVER) + "# edited\n")


class DevRunTest(BuckTestCase):
    @dev_test()
    async def test_args_reach_the_server(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//run:plain", "none", "--", "one", "--two")
        try:
            first = await dev.wait_for("started")
            assert first["argv"] == ["--from-rule", "one", "--two"], first["argv"]

            # They are the same for the whole session, also after a restart.
            edit_server(buck)
            second = await dev.wait_for("started")
            assert second["pid"] != first["pid"]
            assert second["argv"] == ["--from-rule", "one", "--two"], second["argv"]
        finally:
            await dev.stop()

    @dev_test()
    async def test_server_has_the_client_environment(self, buck: Buck) -> None:
        dev = await start_dev(
            buck, "//run:plain", env={"DEV_TEST_VAR": "from the client"}
        )
        try:
            started = await dev.wait_for("started")
            assert started["env"].get("DEV_TEST_VAR") == "from the client"
            assert "BUCK2_DEV_SOCKET" in started["env"]
        finally:
            await dev.stop()

    @dev_test()
    async def test_run_info_alone_is_the_server(self, buck: Buck) -> None:
        dev = await start_dev(buck, "//run:run_only", "none", "--", "arg")
        try:
            first = await dev.wait_for("started")
            assert first["argv"] == ["arg"], first["argv"]

            # The server's command names the script, so new contents start it again.
            edit_server(buck)
            second = await dev.wait_for("started")
            assert second["pid"] != first["pid"]
            await wait_until(
                lambda: not alive(first["pid"]), "the first server is stopped"
            )
            assert dev.running()
        finally:
            await dev.stop()
        assert not alive(second["pid"])

    @dev_test()
    async def test_target_with_neither_provider(self, buck: Buck) -> None:
        await expect_failure(
            buck.run_buck_command("dev", "//run:neither"),
            stderr_regex="has no `DevInfo` provider and no `RunInfo` provider",
        )
