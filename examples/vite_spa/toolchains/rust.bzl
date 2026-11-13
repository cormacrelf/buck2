# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

load("@prelude//rust:rust_toolchain.bzl", "PanicRuntime", "RustExplicitSysrootDeps", "RustToolchainInfo")

def _rust_toolchain_impl(ctx: AnalysisContext) -> list[Provider]:
    return [
        DefaultInfo(),
        RustToolchainInfo(
            # rustc compiles; the C++ toolchain links, as it does a `cxx_binary`.
            advanced_unstable_linking = True,
            allow_lints = ctx.attrs.allow_lints,
            clippy_driver = RunInfo(args = ["clippy-driver"]),
            compiler = RunInfo(args = ["rustc"]),
            default_edition = ctx.attrs.default_edition,
            deny_lints = ctx.attrs.deny_lints,
            # The standard library as targets (see rust_sysroot.bzl). advanced_unstable_linking
            # needs it: rustc gets an empty sysroot, and the C++ linker links the rlibs.
            explicit_sysroot_deps = RustExplicitSysrootDeps(
                core = ctx.attrs.sysroot_core,
                proc_macro = ctx.attrs.sysroot_proc_macro,
                std = ctx.attrs.sysroot_std,
                panic_unwind = ctx.attrs.sysroot_panic_unwind,
                panic_abort = ctx.attrs.sysroot_panic_abort,
                others = ctx.attrs.sysroot_others,
            ),
            # must match the target's panic strategy, from `rustc --print target-spec-json`
            panic_runtime = PanicRuntime(ctx.attrs.panic_runtime),
            # advanced_unstable_linking and the explicit sysroot deps need it. The prelude then
            # passes RUSTC_BOOTSTRAP=1 to rustc and rustdoc.
            nightly_features = True,
            # The prelude passes the explicit sysroot deps as `--extern noprelude:...`, which is
            # unstable.
            rustc_flags = ["-Zunstable-options"] + ctx.attrs.rustc_flags,
            rustc_target_triple = ctx.attrs.rustc_target_triple,
            rustdoc = RunInfo(args = ["rustdoc"]),
            warn_lints = ctx.attrs.warn_lints,
        ),
    ]

# rustc and rustdoc from the PATH, with the standard library of `sysroot_*`.
rust_toolchain = rule(
    impl = _rust_toolchain_impl,
    attrs = {
        "allow_lints": attrs.list(attrs.string(), default = []),
        "default_edition": attrs.option(attrs.string(), default = None),
        "deny_lints": attrs.list(attrs.string(), default = []),
        "panic_runtime": attrs.enum(PanicRuntime.values()),
        "rustc_flags": attrs.list(attrs.arg(), default = []),
        "rustc_target_triple": attrs.string(),
        "sysroot_core": attrs.dep(),
        "sysroot_others": attrs.list(attrs.dep(), default = []),
        "sysroot_panic_abort": attrs.dep(),
        "sysroot_panic_unwind": attrs.dep(),
        "sysroot_proc_macro": attrs.dep(),
        "sysroot_std": attrs.dep(),
        "warn_lints": attrs.list(attrs.string(), default = []),
    },
    is_toolchain_rule = True,
)
