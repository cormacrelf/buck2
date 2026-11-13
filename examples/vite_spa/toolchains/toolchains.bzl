# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# Rust and C++ for the configuration's cpu and os: wasm32-unknown-unknown for the browser
# (`-m browser`), the host's triple otherwise. rustc compiles and the C++ toolchain links
# (`advanced_unstable_linking`), so a `rust_binary` for the browser is linked by wasm-ld.
#
# rustc, clang and wasm-ld come from the PATH, and the standard library from the sysroot of that
# rustc. The repo's nix dev shell has all of them, with the standard library for wasm32.

load("@prelude//toolchains:demo.bzl", "android_hack_alias")
load(":cxx.bzl", _cxx_toolchain = "cxx_toolchain")
load(":rust.bzl", _rust_toolchain = "rust_toolchain")
load(":rust_sysroot.bzl", "PANIC_RUNTIME", "SYSROOT_EXTERN_FLAGS", "rust_sysroot", "rust_sysroot_crate")
load(":rustc_target_triple.bzl", "RUSTC_TARGET_TRIPLE")

# What rustc passes to wasm-ld for every wasm32 target: a 1 MiB stack, before the data, so that a
# stack overflow traps instead of overwriting it; and the names in the name section left mangled,
# for wasm-bindgen to demangle.
_WASM_LINK_FLAGS = ["-z", "stack-size=1048576", "--stack-first", "--no-demangle"]

def cxx_toolchain():
    """toolchains//:cxx, and toolchains//:android-hack, which the prelude's android rules use"""
    _cxx_toolchain(
        name = "cxx",
        linker = select({
            "prelude//cpu:wasm32": "wasm-ld",
            "DEFAULT": "clang++",
        }),
        link_flags = select({
            # wasm-ld removes unused sections by default.
            "prelude//cpu:wasm32": _WASM_LINK_FLAGS,
            "prelude//os:linux": ["-fuse-ld=lld"],
            "DEFAULT": [],
        }),
        linker_type = select({
            "prelude//cpu:wasm32": "wasm",
            "prelude//os:macos": "darwin",
            "DEFAULT": "gnu",
        }),
        platform_name = RUSTC_TARGET_TRIPLE,
        visibility = ["PUBLIC"],
    )

    android_hack_alias(
        name = "android-hack",
        actual = ":cxx",
        visibility = ["PUBLIC"],
    )

def _rust_sysroot_crate(name, link, deps = [], **kwargs):
    rust_sysroot_crate(
        name = "rust-sysroot-" + name,
        crate = name,
        link = link,
        deps = [":rust-sysroot-" + d for d in deps],
        sysroot = ":rust-sysroot",
        **kwargs
    )

def _rust_sysroot():
    """The standard library crates, toolchains//:rust-sysroot-<crate>, from the sysroot's rlibs"""
    rust_sysroot(
        name = "rust-sysroot",
        rustc_target_triple = RUSTC_TARGET_TRIPLE,
    )

    _rust_sysroot_crate("core", link = ["core"])
    _rust_sysroot_crate("compiler_builtins", link = ["compiler_builtins"], deps = ["core"])
    _rust_sysroot_crate("alloc", link = ["alloc"], deps = ["core"])
    _rust_sysroot_crate(
        "std",
        link = ["std"],
        link_std = True,
        deps = ["alloc", "core"],
        # Rust std's native libraries, from `rustc --print native-static-libs`. rustc would add
        # these itself, but the C++ linker does the link.
        exported_post_linker_flags = select({
            "prelude//cpu:wasm32": [],
            "prelude//os:macos": ["-lSystem", "-lc", "-lm"],
            "DEFAULT": ["-lgcc_s", "-lutil", "-lrt", "-lpthread", "-lm", "-ldl", "-lc"],
        }),
    )
    _rust_sysroot_crate("panic_unwind", link = ["panic_unwind"], deps = ["alloc", "core"])
    _rust_sysroot_crate("panic_abort", link = ["panic_abort"], deps = ["core"])
    _rust_sysroot_crate("proc_macro", link = ["proc_macro", "rustc_literal_escaper"], deps = ["std"])
    _rust_sysroot_crate("test", link = ["test", "getopts"], deps = ["std"])

def rust_toolchain():
    """toolchains//:rust and the standard library crates it uses"""
    _rust_sysroot()

    _rust_toolchain(
        name = "rust",
        default_edition = "2024",
        panic_runtime = PANIC_RUNTIME,
        rustc_flags = SYSROOT_EXTERN_FLAGS,
        rustc_target_triple = RUSTC_TARGET_TRIPLE,
        sysroot_core = ":rust-sysroot-core",
        sysroot_panic_abort = ":rust-sysroot-panic_abort",
        sysroot_panic_unwind = ":rust-sysroot-panic_unwind",
        sysroot_proc_macro = ":rust-sysroot-proc_macro",
        sysroot_std = ":rust-sysroot-std",
        # for `extern crate alloc;`, the `extern crate compiler_builtins;` that rustc adds to
        # no_std crates, and the `extern crate test;` of test harnesses
        sysroot_others = [":rust-sysroot-alloc", ":rust-sysroot-compiler_builtins", ":rust-sysroot-test"],
        visibility = ["PUBLIC"],
    )
