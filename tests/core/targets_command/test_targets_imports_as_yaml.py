# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# pyre-strict


import ast
import json
from typing import Any

from buck2.tests.e2e_util.api.buck import Buck
from buck2.tests.e2e_util.asserts import expect_failure
from buck2.tests.e2e_util.buck_workspace import buck_test, BuckTestCase


def extract_test_output(stderr: str) -> dict[str, Any]:
    """Extract the DATA_LOAD_TEST_OUTPUT dict from buck2 stderr."""
    marker = "DATA_LOAD_TEST_OUTPUT: "
    for line in stderr.splitlines():
        idx = line.find(marker)
        if idx != -1:
            return ast.literal_eval(line[idx + len(marker) :])  # type: ignore[no-any-return]
    raise AssertionError(f"Could not find '{marker}' in stderr:\n{stderr}")


class TargetsImportsAsYamlTest(BuckTestCase):
    @buck_test()
    async def test_load_yaml_values(self, buck: Buck) -> None:
        """`.yaml` and `.yml` files load by extension, and `?as=yaml` loads a
        file whose extension says nothing. The values map like JSON."""
        result = await buck.targets("root//:")
        data = extract_test_output(result.stderr)

        lock = data["lock"]
        assert lock["lockfileVersion"] == "9.0"
        assert lock["settings"] == {
            "autoInstallPeers": True,
            "excludeLinksFromLockfile": False,
        }
        assert lock["importers"]["."]["dependencies"]["left-pad"] == {
            "specifier": "^1.3.0",
            "version": "1.3.0",
        }
        assert lock["packages"]["left-pad@1.3.0"]["resolution"] == {
            "integrity": "sha512-XI5MPzVNApjAyhQzphX8BkmKsKUxD4LdyK24iZeQEhTx0lcjTV1E3RRf0WYqY+PWh7Vf+cs5jkLjxh9ow2yAfQ=="
        }
        assert lock["snapshots"] == {"left-pad@1.3.0": {}}

        assert data["config"] == {
            "country": "no",
            "enabled": True,
            "count": 42,
            "ratio": 0.5,
            "nothing": None,
            "1": "quoted keys are strings",
            "defaults": {"retries": 3},
            "service": {"retries": 3, "name": "web"},
        }

        assert data["deps"] == [
            {"name": "foo", "version": "1.0.0"},
            {"name": "bar", "version": "2.0.0"},
        ]

    @buck_test()
    async def test_imports_as_yaml(self, buck: Buck) -> None:
        """`targets --imports` reports YAML files as leaf imports, with the
        `?as=yaml` hint stripped from the path."""
        result = await buck.targets(
            "root//:", "--json", "--streaming", "--imports"
        )
        imports = {
            x["buck.file"]: x["buck.imports"]
            for x in json.loads(result.stdout)
            if "buck.imports" in x
        }
        assert set(imports["root//TARGETS.fixture"]) >= {
            "root//config.yml",
            "root//deps.lock",
            "root//pnpm-lock.yaml",
        }
        for leaf in ["root//config.yml", "root//deps.lock", "root//pnpm-lock.yaml"]:
            assert imports[leaf] == [], leaf

    @buck_test()
    async def test_yaml_non_string_key_rejected(self, buck: Buck) -> None:
        """A mapping key that YAML reads as an integer is an error, not
        silently turned into a string."""
        await expect_failure(
            buck.targets("root//bad:"),
            stderr_regex=r"(?s)int_key\.yaml.*string mapping key",
        )
