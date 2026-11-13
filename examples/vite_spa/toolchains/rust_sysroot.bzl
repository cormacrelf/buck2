# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# The standard library crates of rustc's sysroot, as targets for the Rust toolchain's
# `explicit_sysroot_deps`. The prelude needs these when `advanced_unstable_linking` is on: rustc
# then gets an empty sysroot, the standard library's rlibs and .rmeta files reach rustc through
# `--extern` and `-Ldependency`, and its rlibs reach the C++ linker through the native link graph.
#
# The prelude expects the sysroot deps to be Rust libraries built from source, with their metadata
# in the rlib. A prebuilt sysroot keeps it in a separate .rmeta, which rustc must be given with its
# own `--extern` (SYSROOT_EXTERN_FLAGS).
#
# The rlibs are prebuilt, in the sysroot of the `rustc` on the PATH. `rust_sysroot` copies those of
# the configuration's target triple into buck-out. Each `rust_sysroot_crate` gives one crate the
# providers of a Rust library: rustc can find every rlib of the sysroot from each crate, and each
# crate links a list of these rlibs.

load("@prelude//cxx:cxx_toolchain_types.bzl", "CxxToolchainInfo")
load("@prelude//decls:toolchains_common.bzl", "toolchains_common")
load("@prelude//linking:link_groups.bzl", "merge_link_group_lib_info")
load(
    "@prelude//linking:link_info.bzl",
    "Archive",
    "ArchiveLinkable",
    "LibOutputStyle",
    "LinkInfo",
    "LinkInfos",
    "LinkStrategy",
    "MergedLinkInfo",
    "create_merged_link_info",
)
load(
    "@prelude//linking:linkable_graph.bzl",
    "create_linkable_graph",
    "create_linkable_graph_node",
    "create_linkable_node",
)
load(
    "@prelude//linking:shared_libraries.bzl",
    "SharedLibraryInfo",
    "merge_shared_libraries",
)
load("@prelude//linking:types.bzl", "Linkage")
load("@prelude//rust:attrs.bzl", "RustAnalyzerTargetKind")
load("@prelude//rust:build_params.bzl", "MetadataKind")
load("@prelude//rust:crate_name.bzl", "CrateName")
load(
    "@prelude//rust:link_info.bzl",
    "RustArtifact",
    "RustExportedLinkDeps",
    "RustLinkInfo",
    "RustLinkStrategyInfo",
    "RustLinkableGraphs",
    "RustNativeLinkDeps",
    "TransitiveDeps",
)
load("@prelude//rust/rust-analyzer:provider.bzl", "RustAnalyzerInfo")
load("@prelude//third-party:providers.bzl", "third_party_build_info")

# The rlibs in lib/rustlib/<triple>/lib of the sysroot. The copy action fails when the sysroot has
# an rlib that is not listed, or lacks one that is, so a new Rust version that changes the set
# fails there rather than at link time.
_COMMON = [
    "addr2line",
    "adler2",
    "alloc",
    "cfg_if",
    "compiler_builtins",
    "core",
    "getopts",
    "gimli",
    "hashbrown",
    "libc",
    "memchr",
    "miniz_oxide",
    "object",
    "panic_abort",
    "panic_unwind",
    "proc_macro",
    "rustc_demangle",
    "rustc_literal_escaper",
    "rustc_std_workspace_alloc",
    "rustc_std_workspace_core",
    "rustc_std_workspace_std",
    "std",
    "std_detect",
    "sysroot",
    "test",
    "unwind",
]

_HOST = _COMMON + ["profiler_builtins"]

SYSROOT_CRATES = {
    "aarch64-apple-darwin": _HOST,
    "aarch64-unknown-linux-gnu": _HOST,
    "wasm32-unknown-unknown": _COMMON + ["dlmalloc"],
    "x86_64-apple-darwin": _HOST,
    "x86_64-unknown-linux-gnu": _HOST,
}

# Crates that some other crate links (std links itself), or that no program links.
_NOT_IN_STD = [
    "alloc",
    "compiler_builtins",
    "core",
    "getopts",
    "panic_abort",
    "panic_unwind",
    "proc_macro",
    "profiler_builtins",
    "rustc_literal_escaper",
    "rustc_std_workspace_std",
    "std",
    "sysroot",
    "test",
]

# The panic runtime of the target, from `rustc --print target-spec-json`.
# wasm32-unknown-unknown aborts on panic.
PANIC_RUNTIME = select({
    "prelude//cpu:wasm32": "abort",
    "DEFAULT": "unwind",
})

def _extern(crate: str, options: str = "noprelude,nounused") -> str:
    return "--extern={}:{}=$(location :rust-sysroot-{}[rmeta])".format(options, crate, crate)

# rustc flags that pass the .rmeta of each explicit sysroot dep, beside the rlib that the prelude
# passes. The options and the panic runtime are those the prelude uses for the same crates
# (gather_explicit_sysroot_deps).
SYSROOT_EXTERN_FLAGS = [
    _extern("core"),
    _extern("std", "noprelude,nounused,force"),
    _extern("proc_macro"),
    _extern("alloc"),
    _extern("compiler_builtins"),
    _extern("test"),
] + select({
    "prelude//cpu:wasm32": [_extern("panic_abort")],
    "DEFAULT": [_extern("panic_unwind")],
})

# Copies lib<crate>-<hash>.rlib and .rmeta to the given paths, for each crate, from the sysroot of
# the `rustc` on the PATH. The sysroot's rlibs hold only a stub of the crate metadata; the full
# metadata is in the .rmeta.
_COPY = """\
set -eu
triple=$1
shift
libdir="$(rustc --print sysroot)/lib/rustlib/$triple/lib"
if [ ! -d "$libdir" ]; then
  echo "rustc's sysroot has no standard library for $triple: $libdir is missing" >&2
  exit 1
fi
count=0
while [ $# -gt 0 ]; do
  found=""
  for f in "$libdir/lib$1"-*.rlib; do
    if [ ! -e "$f" ]; then
      echo "$libdir has no lib$1-*.rlib: update SYSROOT_CRATES in toolchains/rust_sysroot.bzl" >&2
      exit 1
    fi
    if [ -n "$found" ]; then
      echo "$libdir has more than one lib$1-*.rlib" >&2
      exit 1
    fi
    found=$f
  done
  cp "$found" "$2"
  cp "${found%.rlib}.rmeta" "$3"
  count=$((count + 1))
  shift 3
done
total=$(ls "$libdir" | grep -c '\\.rlib$')
if [ "$total" -ne "$count" ]; then
  echo "$libdir has $total rlibs, but SYSROOT_CRATES in toolchains/rust_sysroot.bzl lists $count" >&2
  exit 1
fi
"""

RustSysrootRlibsInfo = provider(fields = {
    # the rlibs and .rmeta files of every crate, for rustc to find the dependencies of a crate
    "all": TransitiveDeps,
    # crate name -> the rlib, in a 1-element set
    "rlib_singletons": dict[str, TransitiveDeps],
    # crate name -> rlib, for the linker
    "rlibs": dict[str, Artifact],
    # crate name -> rmeta, for rustc
    "rmetas": dict[str, Artifact],
    # The crates whose rlibs a program that uses std links, besides those of core and alloc.
    "std_link": list[str],
})

def _singletons(ctx: AnalysisContext, artifacts: dict[str, Artifact]) -> dict[str, TransitiveDeps]:
    return {
        crate: ctx.actions.tset(
            TransitiveDeps,
            value = RustArtifact(artifact = artifact, crate = CrateName(simple = crate, dynamic = None)),
        )
        for crate, artifact in artifacts.items()
    }

def _rust_sysroot_impl(ctx: AnalysisContext) -> list[Provider]:
    triple = ctx.attrs.rustc_target_triple
    if triple not in SYSROOT_CRATES:
        fail("no standard library crates are listed for {}".format(triple))
    rlibs = {}
    rmetas = {}
    cmd = cmd_args("sh", "-c", _COPY, "--", triple)
    for crate in SYSROOT_CRATES[triple]:
        rlibs[crate] = ctx.actions.declare_output("lib{}.rlib".format(crate))
        rmetas[crate] = ctx.actions.declare_output("lib{}.rmeta".format(crate))
        cmd.add(crate, rlibs[crate].as_output(), rmetas[crate].as_output())
    ctx.actions.run(cmd, category = "rust_sysroot_copy")

    rlib_singletons = _singletons(ctx, rlibs)
    rmeta_singletons = _singletons(ctx, rmetas)
    return [
        DefaultInfo(default_outputs = rlibs.values() + rmetas.values()),
        RustSysrootRlibsInfo(
            rlibs = rlibs,
            rmetas = rmetas,
            all = ctx.actions.tset(TransitiveDeps, children = rlib_singletons.values() + rmeta_singletons.values()),
            rlib_singletons = rlib_singletons,
            std_link = [c for c in rlibs if c not in _NOT_IN_STD],
        ),
    ]

rust_sysroot = rule(
    impl = _rust_sysroot_impl,
    attrs = {
        "rustc_target_triple": attrs.string(),
    },
)

def _rust_sysroot_crate_impl(ctx: AnalysisContext) -> list[Provider]:
    sysroot = ctx.attrs.sysroot[RustSysrootRlibsInfo]
    cxx_toolchain = ctx.attrs._cxx_toolchain[CxxToolchainInfo]
    linker_type = cxx_toolchain.linker_info.type
    crate = ctx.attrs.crate
    rlib = sysroot.rlibs[crate]
    deps = ctx.attrs.deps

    link_info = LinkInfo(
        name = crate,
        linkables = [
            ArchiveLinkable(
                archive = Archive(artifact = sysroot.rlibs[c]),
                linker_type = linker_type,
                supports_lto = False,
            )
            for c in ctx.attrs.link + (sysroot.std_link if ctx.attrs.link_std else [])
        ],
        post_flags = ctx.attrs.exported_post_linker_flags,
    )

    # The rlibs work for both, as rustc builds the sysroot position independent.
    link_infos = {
        LibOutputStyle("archive"): LinkInfos(default = link_info),
        LibOutputStyle("pic_archive"): LinkInfos(default = link_info),
    }
    merged_link_info = create_merged_link_info(
        ctx,
        cxx_toolchain.pic_behavior,
        link_infos,
        deps = [d[MergedLinkInfo] for d in deps],
        preferred_linkage = Linkage("static"),
    )
    shared_library_info = merge_shared_libraries(ctx.actions, deps = [d[SharedLibraryInfo] for d in deps])
    linkable_graph = create_linkable_graph(
        ctx,
        node = create_linkable_graph_node(
            ctx,
            linkable_node = create_linkable_node(
                ctx = ctx,
                default_soname = None,
                preferred_linkage = Linkage("static"),
                deps = deps,
                link_infos = link_infos,
            ),
        ),
        deps = deps,
    )
    rust_infos = [d[RustLinkInfo] for d in deps]
    third_party = third_party_build_info(ctx.actions, children = [i.third_party_build_info for i in rust_infos])

    # The prelude passes the rlib with `--extern`; SYSROOT_EXTERN_FLAGS pass the .rmeta. rustc
    # finds the dependencies of these crates in the -Ldependency directory, which holds both files
    # of each.
    metadata_kinds = [MetadataKind("fast"), MetadataKind("full"), MetadataKind("link")]
    strategy = RustLinkStrategyInfo(
        outputs = {m: rlib for m in metadata_kinds},
        singleton_tset = {m: sysroot.rlib_singletons[crate] for m in metadata_kinds},
        transitive_deps = {m: sysroot.all for m in metadata_kinds},
        transitive_proc_macro_deps = set(),
        rust_debug_info = None,
    )

    return [
        DefaultInfo(
            default_output = rlib,
            sub_targets = {"rmeta": [DefaultInfo(default_output = sysroot.rmetas[crate])]},
        ),
        merged_link_info,
        shared_library_info,
        linkable_graph,
        third_party,
        merge_link_group_lib_info(deps = deps),
        RustLinkInfo(
            crate = CrateName(simple = crate, dynamic = None),
            strategies = {s: strategy for s in LinkStrategy},
            native_link_deps = ctx.actions.tset(
                RustNativeLinkDeps,
                children = [i.native_link_deps for i in rust_infos] + [
                    ctx.actions.tset(RustNativeLinkDeps, value = [(ctx.label.configured_target(), merged_link_info)]),
                ],
            ),
            linkable_graphs = ctx.actions.tset(
                RustLinkableGraphs,
                children = [i.linkable_graphs for i in rust_infos] + [
                    ctx.actions.tset(RustLinkableGraphs, value = [linkable_graph]),
                ],
            ),
            shared_libs = shared_library_info,
            third_party_build_info = third_party,
            exported_link_deps = ctx.actions.tset(RustExportedLinkDeps, children = [i.exported_link_deps for i in rust_infos]),
        ),
        # rust-analyzer gets the standard library from the sysroot's sources, not from the crate
        # graph, so these crates are left out of it.
        RustAnalyzerInfo(
            available_proc_macros = [],
            crate = CrateName(simple = crate, dynamic = None),
            crate_root = "",
            edition = "2024",
            env = {},
            features = [],
            rust_deps = [],
            rustc_flags = cmd_args(),
            target_kind = RustAnalyzerTargetKind("lib"),
            transitive_target_set = set(),
        ),
    ]

rust_sysroot_crate = rule(
    impl = _rust_sysroot_crate_impl,
    attrs = {
        "crate": attrs.string(),
        "deps": attrs.list(attrs.dep(providers = [RustLinkInfo]), default = []),
        "exported_post_linker_flags": attrs.list(attrs.string(), default = []),
        # create_linkable_node reads it
        "labels": attrs.list(attrs.string(), default = []),
        "link": attrs.list(attrs.string(), doc = "the crates whose rlibs this crate links"),
        "link_std": attrs.bool(default = False, doc = "also link the crates std depends on"),
        "sysroot": attrs.dep(providers = [RustSysrootRlibsInfo]),
        "_cxx_toolchain": toolchains_common.cxx(),
    },
)
