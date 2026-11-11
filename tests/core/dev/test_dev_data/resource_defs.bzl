# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# Rules for `test_dev_resources`: local resources, and a `DevInfo` that uses them.

def _local_resource(ctx):
    return [
        DefaultInfo(),
        LocalResourceInfo(
            # The setup reads `version`: see `resources/setup.py`.
            setup = cmd_args("python3", ctx.attrs.setup, ctx.label.name, ctx.attrs.version),
            resource_env_vars = ctx.attrs.env,
            setup_timeout_seconds = ctx.attrs.timeout,
        ),
    ]

local_resource = rule(
    impl = _local_resource,
    attrs = {
        "env": attrs.dict(attrs.string(), attrs.string()),
        "setup": attrs.source(),
        "timeout": attrs.int(default = 30),
        "version": attrs.source(),
    },
)

def _dev_with_resources(ctx):
    # An output that a change to `page` builds again, and that nothing else uses.
    page = ctx.actions.copy_file("page.txt", ctx.attrs.page)
    return [
        DefaultInfo(),
        DevInfo(
            server = cmd_args("python3", ctx.attrs.server),
            outputs = {"page": page},
            local_resources = {name: dep.label for name, dep in ctx.attrs.resources.items()},
        ),
    ]

dev_with_resources = rule(
    impl = _dev_with_resources,
    attrs = {
        "page": attrs.source(),
        "resources": attrs.dict(attrs.string(), attrs.dep(providers = [LocalResourceInfo])),
        "server": attrs.source(),
    },
)
