# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

load("@prelude//cfg/modifier:name.bzl", "cfg_name")

# What a browser runs: wasm32, with no operating system. JavaScript is data to it, as a page's
# images are, and is the same for every configuration; a wasm module is built for this one.
BROWSER = ["prelude//cpu:wasm32", "prelude//os:none"]

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

def _modifier_transition_impl(ctx):
    settings = {}
    for dep in ctx.attrs.constraints:
        settings.update(dep[ConfigurationInfo].constraints)

    def impl(platform: PlatformInfo) -> PlatformInfo:
        constraints = dict(platform.configuration.constraints)
        constraints.update(settings)
        configuration = ConfigurationInfo(constraints = constraints, values = platform.configuration.values)

        # Named as the modifiers name a configuration, so that a target reached through this
        # transition is in the same configuration as when it is built with these modifiers.
        return PlatformInfo(label = cfg_name(configuration), configuration = configuration)

    return [DefaultInfo(), TransitionInfo(impl = impl)]

# A transition that applies `constraints`, constraint values or config settings, as modifiers would, to the configuration
# it is given, keeping its other constraints.
modifier_transition = rule(
    impl = _modifier_transition_impl,
    attrs = {"constraints": attrs.list(attrs.dep(providers = [ConfigurationInfo]))},
    is_configuration_rule = True,
)
