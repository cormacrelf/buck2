# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# A rule that runs a Rust binary under `buck2 dev`, and patches one of its crates while it
# runs with subsecond (https://crates.io/crates/subsecond).
#
# Only Linux is supported: ThinLink, the toolchain's linker, handles ELF only.

def _subsecond_app_impl(ctx):
    exe = ctx.attrs.binary[DefaultInfo].default_outputs[0]

    # The crate as a shared library. ThinLink, the toolchain's linker, links it as a patch when
    # the crate sets `SUBSECOND_LINK = "thin"` in its `env`.
    patch = ctx.attrs.lib[DefaultInfo].sub_targets["cdylib"][DefaultInfo].default_outputs[0]

    server = cmd_args(
        "python3",
        ctx.attrs.server,
        "--thinlink",
        ctx.attrs._thinlink[RunInfo],
    )
    return [
        DefaultInfo(default_output = exe, other_outputs = [patch]),
        DevInfo(
            server = server,
            outputs = {"exe": exe, "patch": patch},
            watch = [
                # A change to the server script or to ThinLink restarts the server, and so the
                # app, which the new server builds again.
                DevWatch(inputs = server, restart = True),
                # A change to the patched crate builds a patch. The patch does not depend on
                # the binary, so the binary stays the one that is running.
                DevWatch(targets = [ctx.attrs.lib.label], build = ["patch"]),
                # A change to any other crate cannot be patched: build the binary again. The
                # server starts it again.
                DevWatch(targets = None, build = ["exe"]),
            ],
        ),
    ]

# `buck2 dev` on this target runs `binary`, and patches the code of `lib` into it when a
# source file of `lib` changes. `binary` must depend on `lib`, and set
# `env = {"SUBSECOND_LINK": "fat"}`; `lib` must set `env = {"SUBSECOND_LINK": "thin"}`.
subsecond_app = rule(
    impl = _subsecond_app_impl,
    attrs = {
        "binary": attrs.dep(providers = [RunInfo]),
        "lib": attrs.dep(),
        "server": attrs.source(),
        "_thinlink": attrs.exec_dep(providers = [RunInfo], default = "toolchains//:thinlink"),
    },
)
