# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# Browser modules built by buck2, one action per module, for a Vite dev server that `buck2 dev`
# feeds (see `dev.bxl` and `server/server.mjs`).
#
# - `vite_package` is a pnpm workspace package compiled for the browser. Each source, and each
#   way of importing it (`?raw`, `?url`, `?inline`, `?worker`), is a module of its own, compiled
#   by `tools/transform.mjs` with its imports resolved to URLs. Its dependencies come from the
#   lockfile, as for `pnpm_package`.
# - `vite_npm` pre-bundles each package of the npm store into ES modules, with
#   `tools/npm_esm.mjs`, one action per package.

load("@prelude//nodejs:nodejs_npm_store.bzl", "NodejsNpmStoreInfo", "npm_store_name")
load("@prelude//nodejs:nodejs_providers.bzl", "NodejsLinksInfo")
load("@prelude//nodejs:pnpm.bzl", "pnpm_importer")

ViteModulesInfo = provider(fields = {
    # The package's directory in the project.
    "package": provider_field(str),
    "package_name": provider_field(str | None),
    # The module that each subpath the package exports names, as a path in the project.
    "exports": provider_field(dict[str, str]),
    # Each module by its key: the source's path in the project, with `~<query>` for a query.
    "modules": provider_field(dict[str, Artifact]),
    # `index.html`, with its paths made absolute in the project, for a package that has one.
    "html": provider_field(Artifact | None),
    # The directory Vite serves as it is, as a path in the project.
    "public": provider_field(str | None),
})

ViteNpmInfo = provider(fields = {
    # The pre-bundled ES modules of each npm package, by its entry's name in the store.
    "esm": provider_field(dict[str, Artifact]),
})

_JS = (".ts", ".tsx", ".mts", ".js", ".jsx", ".mjs")
_CSS = (".css", ".scss", ".sass")

def _queries(path: str) -> list[str]:
    """The ways a source can be imported, each a module of its own. Actions are only declared:
    a module that nothing asks for is never built."""
    if path.endswith(".d.ts"):
        return []
    if path.endswith(_JS):
        return ["", "worker", "url", "raw"]
    if path.endswith(_CSS):
        return ["", "inline", "raw"]
    if path.endswith(".json"):
        return ["", "raw", "url"]
    return ["", "url", "raw", "inline"]

def _vite_library_impl(ctx: AnalysisContext) -> list[Provider]:
    pkg = ctx.label.package
    srcs = {}
    for src in ctx.attrs.srcs:
        srcs[pkg + "/" + src.short_path] = src
    for path, dep in ctx.attrs.generated.items():
        srcs[pkg + "/" + path] = dep[DefaultInfo].default_outputs[0]

    workspace = {}
    for dep in ctx.attrs.deps:
        info = dep[ViteModulesInfo]
        if info.package_name:
            workspace[info.package_name] = {"exports": info.exports, "package": info.package}
    npm = {}
    for dep in ctx.attrs.node_modules:
        for link in dep[NodejsLinksInfo].links:
            npm[link.name] = npm_store_name(link.snapshot)

    resolve_ctx = ctx.actions.write_json("vite/ctx.json", {
        "aliases": {k: pkg + "/" + v for k, v in ctx.attrs.aliases.items()},
        "files": sorted(srcs.keys()),
        "npm": npm,
        "package": pkg,
        "workspace": workspace,
    })

    # Sass partials and CSS `@import`s are read from the source tree, so a stylesheet is built
    # with every stylesheet of the package.
    styles = [a for p, a in srcs.items() if p.endswith(_CSS)]
    tool = ctx.attrs._transform[RunInfo]
    modules = {}
    for path, src in srcs.items():
        for query in _queries(path):
            key = path + ("~" + query if query else "")
            out = ctx.actions.declare_output("vite/m/" + key[len(pkg) + 1:] + ".js")
            ctx.actions.run(
                cmd_args(
                    tool,
                    "--ctx",
                    resolve_ctx,
                    "--path",
                    path,
                    "--query=" + query,
                    "--src",
                    src,
                    "--out",
                    out.as_output(),
                    hidden = styles if path.endswith(_CSS) else [],
                ),
                category = "vite_transform",
                identifier = key[len(pkg) + 1:],
            )
            modules[key] = out

    exports = {k: pkg + "/" + v for k, v in ctx.attrs.exports.items()}

    html = None
    if ctx.attrs.html:
        html = ctx.actions.declare_output("vite/index.html")
        ctx.actions.run(
            cmd_args(ctx.attrs._html[RunInfo], "--package", pkg, "--src", ctx.attrs.html, "--out", html.as_output()),
            category = "vite_html",
        )

    return [
        DefaultInfo(default_outputs = [modules[p] for p in srcs if p in modules]),
        ViteModulesInfo(
            package = pkg,
            package_name = ctx.attrs.package_name,
            exports = exports,
            modules = modules,
            html = html,
            public = pkg + "/" + ctx.attrs.public if ctx.attrs.public else None,
        ),
    ]

vite_library = rule(
    impl = _vite_library_impl,
    attrs = {
        # Import aliases, as Vite's `resolve.alias` and tsconfig's `paths`: `{"@/": "src/"}`.
        "aliases": attrs.dict(attrs.string(), attrs.string(), default = {}),
        "deps": attrs.list(attrs.dep(providers = [ViteModulesInfo]), default = []),
        # The module each subpath that the package exports names, relative to the package.
        # This is where a package whose `package.json` exports compiled files (`dist/`) says
        # which source each one is compiled from: the browser gets the source, compiled here.
        "exports": attrs.dict(attrs.string(), attrs.string(), default = {}),
        # Generated sources, by the path in the package they are imported at.
        "generated": attrs.dict(attrs.string(), attrs.dep(), default = {}),
        "html": attrs.option(attrs.source(), default = None),
        "node_modules": attrs.list(attrs.dep(providers = [NodejsLinksInfo]), default = []),
        "package_name": attrs.option(attrs.string(), default = None),
        "public": attrs.option(attrs.string(), default = None),
        "srcs": attrs.list(attrs.source(), default = []),
        "_html": attrs.default_only(attrs.exec_dep(default = "root//vite/tools:html")),
        "_transform": attrs.default_only(attrs.exec_dep(default = "root//vite/tools:transform")),
    },
)

def vite_package(srcs, exports = {}, **kwargs):
    """A `vite_library` for this pnpm workspace package, with the dependencies the lockfile
    gives it, named after its directory as `pnpm_package` names its library."""
    importer = pnpm_importer()
    vite_library(
        name = importer.name,
        package_name = kwargs.pop("package_name", importer.package_name),
        srcs = srcs,
        exports = exports,
        deps = importer.deps,
        node_modules = importer.node_modules,
        visibility = ["PUBLIC"],
        **kwargs
    )

def _vite_npm_impl(ctx: AnalysisContext) -> list[Provider]:
    store = ctx.attrs.store[NodejsNpmStoreInfo]
    tool = ctx.attrs._npm_esm[RunInfo]
    esm = {}
    for snapshot, entry in store.entries.items():
        name = npm_store_name(snapshot)
        out = ctx.actions.declare_output("esm/" + name, dir = True)
        ctx.actions.run(
            cmd_args(tool, "--pkg", entry.project("node_modules/" + store.names[snapshot]), "--out", out.as_output()),
            category = "vite_npm_esm",
            identifier = name,
        )
        esm[name] = out
    return [DefaultInfo(), ViteNpmInfo(esm = esm)]

vite_npm = rule(
    impl = _vite_npm_impl,
    attrs = {
        "store": attrs.dep(providers = [NodejsNpmStoreInfo]),
        "_npm_esm": attrs.default_only(attrs.exec_dep(default = "root//vite/tools:npm_esm")),
    },
)
