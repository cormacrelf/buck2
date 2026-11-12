# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

def _local_impl(ctx):
    constraints = {}
    constraints.update(ctx.attrs.cpu[ConfigurationInfo].constraints)
    constraints.update(ctx.attrs.os[ConfigurationInfo].constraints)
    platform = ExecutionPlatformInfo(
        label = ctx.label.raw_target(),
        configuration = ConfigurationInfo(constraints = constraints, values = {}),
        executor_config = CommandExecutorConfig(
            local_enabled = True,
            remote_enabled = False,
            # The module transforms run in a persistent worker (`vite/tools/worker.mjs`).
            use_persistent_workers = True,
        ),
    )
    return [DefaultInfo(), ExecutionPlatformRegistrationInfo(platforms = [platform])]

# Local execution, with persistent workers.
local_platform = rule(
    impl = _local_impl,
    attrs = {
        "cpu": attrs.dep(providers = [ConfigurationInfo]),
        "os": attrs.dep(providers = [ConfigurationInfo]),
    },
)
