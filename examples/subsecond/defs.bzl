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
# Only Linux is supported: the patch is an ELF shared library, linked with `cc` from your
# `PATH`, and the server reads symbols with `nm`.

def _subsecond_app_impl(ctx):
    exe = ctx.attrs.binary[DefaultInfo].default_outputs[0]

    # The rlib built with `-Crelocation-model=pic`, so that its code can go in a shared
    # library. Its symbols have the same names as those of the rlib in the binary.
    rlib = ctx.attrs.lib[DefaultInfo].sub_targets["static_pic"][DefaultInfo].default_outputs[0]
    patch = ctx.actions.declare_output("patch.so")
    ctx.actions.run(
        cmd_args("python3", ctx.attrs._link_patch, "cc", rlib, patch.as_output()),
        category = "subsecond_link_patch",
        local_only = True,
    )

    return [
        DefaultInfo(default_output = exe, other_outputs = [patch]),
        DevInfo(
            server = cmd_args("python3", ctx.attrs.server),
            outputs = {"exe": exe, "patch": patch},
            watch = [
                # A change to the server script restarts it, and so the app.
                DevWatch(targets = [ctx.attrs.server.owner.raw_target()], restart = True),
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
# source file of `lib` changes. `binary` must depend on `lib`, and be linked with
# `-Wl,--export-dynamic`.
subsecond_app = rule(
    impl = _subsecond_app_impl,
    attrs = {
        "binary": attrs.dep(providers = [RunInfo]),
        "lib": attrs.dep(),
        "server": attrs.source(),
        "_link_patch": attrs.source(default = "root//:link_patch.py"),
    },
)
