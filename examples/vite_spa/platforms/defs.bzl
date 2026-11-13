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
    configuration = ConfigurationInfo(constraints = constraints, values = {})
    platform = ExecutionPlatformInfo(
        label = ctx.label.raw_target(),
        configuration = configuration,
        executor_config = CommandExecutorConfig(
            local_enabled = True,
            remote_enabled = False,
            # The module transforms run in a persistent worker (`vite/tools/worker.mjs`).
            use_persistent_workers = True,
        ),
    )
    return [
        DefaultInfo(),
        ExecutionPlatformRegistrationInfo(platforms = [platform]),
        # The same label and constraints as the execution platform, so that targets and their
        # exec deps share one configuration. With different ones, a package that is both, like
        # the tools that the dev server and every transform use, is built twice, npm store and all.
        PlatformInfo(label = str(ctx.label.raw_target()), configuration = configuration),
    ]

# Local execution, with persistent workers, and the target platform too.
local_platform = rule(
    impl = _local_impl,
    attrs = {
        "cpu": attrs.dep(providers = [ConfigurationInfo]),
        "os": attrs.dep(providers = [ConfigurationInfo]),
    },
)
