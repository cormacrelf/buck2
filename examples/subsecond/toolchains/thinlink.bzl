# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

load("@prelude//toolchains:cxx.bzl", "CxxToolsInfo")

def _thinlink_cxx_tools_impl(ctx):
    base = ctx.attrs.base[CxxToolsInfo]
    return [
        DefaultInfo(),
        CxxToolsInfo(
            compiler = base.compiler,
            compiler_type = base.compiler_type,
            cxx_compiler = base.cxx_compiler,
            asm_compiler = base.asm_compiler,
            asm_compiler_type = base.asm_compiler_type,
            rc_compiler = base.rc_compiler,
            cvtres_compiler = base.cvtres_compiler,
            archiver = base.archiver,
            archiver_type = base.archiver_type,
            linker = cmd_args(ctx.attrs.thinlink[RunInfo], "--driver", base.linker),
            linker_type = base.linker_type,
        ),
    ]

# The C++ tools of `base`, with `thinlink` around its linker.
thinlink_cxx_tools = rule(
    impl = _thinlink_cxx_tools_impl,
    attrs = {
        "base": attrs.exec_dep(providers = [CxxToolsInfo]),
        "thinlink": attrs.exec_dep(providers = [RunInfo]),
    },
)
