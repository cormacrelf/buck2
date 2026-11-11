# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# Rules for `test_dev_run`: servers given as a `DevInfo` without outputs, as a `RunInfo` alone,
# and a target with neither.

def _server(ctx):
    return cmd_args("python3", ctx.attrs.server, ctx.attrs.server_args)

def _plain_dev(ctx):
    return [DefaultInfo(), DevInfo(server = _server(ctx))]

plain_dev = rule(
    impl = _plain_dev,
    attrs = {
        "server": attrs.source(),
        "server_args": attrs.list(attrs.string(), default = []),
    },
)

def _run_only(ctx):
    return [DefaultInfo(), RunInfo(args = _server(ctx))]

run_only = rule(
    impl = _run_only,
    attrs = {
        "server": attrs.source(),
        "server_args": attrs.list(attrs.string(), default = []),
    },
)

def _neither(ctx):
    return [DefaultInfo(default_output = ctx.actions.write("out.txt", ""))]

neither = rule(impl = _neither, attrs = {})
