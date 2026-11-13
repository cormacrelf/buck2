# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

load(
    "@prelude//cxx:cxx_toolchain_types.bzl",
    "BinaryUtilitiesInfo",
    "CCompilerInfo",
    "CvtresCompilerInfo",
    "CxxCompilerInfo",
    "CxxInternalTools",
    "CxxPlatformInfo",
    "CxxToolchainInfo",
    "DepTrackingMode",
    "LinkerInfo",
    "LinkerType",
    "PicBehavior",
    "RcCompilerInfo",
    "RuntimeDependencyHandling",
    "ShlibInterfacesMode",
)
load("@prelude//cxx:headers.bzl", "HeaderMode")
load("@prelude//linking:link_info.bzl", "LinkStyle")
load("@prelude//linking:lto.bzl", "LtoMode")

def _cxx_toolchain_impl(ctx: AnalysisContext) -> list[Provider]:
    linker_type = LinkerType(ctx.attrs.linker_type)
    pic_behavior = PicBehavior("always_enabled" if linker_type == LinkerType("darwin") else "supported")

    def tool(name):
        return RunInfo(args = [name])

    return [
        DefaultInfo(),
        CxxToolchainInfo(
            internal_tools = ctx.attrs.internal_tools[CxxInternalTools],
            linker_info = LinkerInfo(
                linker = tool(ctx.attrs.linker),
                linker_flags = ctx.attrs.link_flags,
                post_linker_flags = [],
                archiver = tool(ctx.attrs.archiver),
                archiver_type = "gnu",
                archiver_supports_argfiles = linker_type != LinkerType("darwin"),
                generate_linker_maps = False,
                lto_mode = LtoMode("none"),
                type = linker_type,
                link_binaries_locally = True,
                link_libraries_locally = True,
                archive_objects_locally = True,
                use_archiver_flags = True,
                static_dep_runtime_ld_flags = [],
                static_pic_dep_runtime_ld_flags = [],
                shared_dep_runtime_ld_flags = [],
                independent_shlib_interface_linker_flags = [],
                shlib_interfaces = ShlibInterfacesMode("disabled"),
                link_style = LinkStyle("static"),
                link_weight = 1,
                binary_extension = "",
                object_file_extension = "o",
                shared_library_name_default_prefix = "lib",
                shared_library_name_format = "{}.so",
                shared_library_versioned_name_format = "{}.so.{}",
                static_library_extension = "a",
                force_full_hybrid_if_capable = False,
                is_pdb_generated = False,
            ),
            bolt_enabled = False,
            binary_utilities_info = BinaryUtilitiesInfo(
                nm = tool("nm"),
                objcopy = tool("objcopy"),
                objdump = tool("objdump"),
                ranlib = tool("ranlib"),
                strip = tool("strip"),
                dwp = None,
            ),
            cxx_compiler_info = CxxCompilerInfo(
                compiler = tool(ctx.attrs.cxx_compiler),
                preprocessor_flags = [],
                compiler_flags = ctx.attrs.compiler_flags,
                compiler_type = "clang",
            ),
            c_compiler_info = CCompilerInfo(
                compiler = tool(ctx.attrs.compiler),
                preprocessor_flags = [],
                compiler_flags = ctx.attrs.compiler_flags,
                compiler_type = "clang",
            ),
            as_compiler_info = CCompilerInfo(
                compiler = tool(ctx.attrs.compiler),
                compiler_type = "clang",
            ),
            asm_compiler_info = CCompilerInfo(
                compiler = tool(ctx.attrs.compiler),
                compiler_type = "clang",
            ),
            cvtres_compiler_info = CvtresCompilerInfo(),
            rc_compiler_info = RcCompilerInfo(),
            header_mode = HeaderMode("symlink_tree_only"),
            cpp_dep_tracking_mode = DepTrackingMode("show_headers"),
            pic_behavior = pic_behavior,
            llvm_link = tool("llvm-link"),
            use_dep_files = True,
            runtime_dependency_handling = RuntimeDependencyHandling("no_symlink"),
        ),
        CxxPlatformInfo(name = ctx.attrs.platform_name),
    ]

# clang, and a linker, from the PATH.
cxx_toolchain = rule(
    impl = _cxx_toolchain_impl,
    attrs = {
        "archiver": attrs.string(default = "ar"),
        "compiler": attrs.string(default = "clang"),
        "compiler_flags": attrs.list(attrs.arg(), default = []),
        "cxx_compiler": attrs.string(default = "clang++"),
        "internal_tools": attrs.default_only(attrs.exec_dep(providers = [CxxInternalTools], default = "prelude//cxx/tools:internal_tools")),
        "link_flags": attrs.list(attrs.arg(), default = []),
        "linker": attrs.string(),
        "linker_type": attrs.enum(LinkerType.values()),
        "platform_name": attrs.string(),
    },
    is_toolchain_rule = True,
)
