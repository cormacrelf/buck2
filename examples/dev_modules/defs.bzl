# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

JsModules = provider(
    fields = {
        # Each compiled module, by the project-relative path of its source file.
        "js": provider_field(dict[str, Artifact]),
    },
)

def _js_library_impl(ctx):
    js = {}
    for src in ctx.attrs.srcs:
        path = ctx.label.package + "/" + src.short_path
        out = ctx.actions.declare_output(src.short_path)
        ctx.actions.run(
            cmd_args("python3", ctx.attrs._compile, path, src, out.as_output()),
            category = "compile",
            identifier = src.short_path,
        )
        js[path] = out
    return [
        DefaultInfo(default_outputs = js.values()),
        JsModules(js = js),
    ]

# ES modules, each compiled by an action of its own. A module imports the others by their paths
# from the project root, such as `/lib/format/format.js`, so `deps` only say which libraries a
# library uses.
js_library = rule(
    impl = _js_library_impl,
    attrs = {
        "deps": attrs.list(attrs.dep(providers = [JsModules]), default = []),
        "srcs": attrs.list(attrs.source()),
        "_compile": attrs.default_only(attrs.source(default = "root//tools:compile.py")),
    },
)
