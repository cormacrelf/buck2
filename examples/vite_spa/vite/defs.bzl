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
load("@prelude//nodejs:nodejs_providers.bzl", "NodejsLibraryInfo", "NodejsLinksInfo")
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
    # What it takes to compile a module that `modules` does not have, such as an image with
    # resize directives that only its importer knows: each source by its path, the stylesheets
    # a stylesheet is compiled with, and the compiler with this package's context and plugins.
    "srcs": provider_field(dict[str, Artifact]),
    "styles": provider_field(list[Artifact]),
    "transform": provider_field(RunInfo),
    # The `--plugins-dir` and `--plugin` args of the build plugins for each extension.
    "plugins": provider_field(dict[str, cmd_args]),
    # The plugins whose server hooks the dev server runs: `--plugins-dir` and `--plugin` args.
    "server_plugins": provider_field(cmd_args | None),
})

ViteNpmInfo = provider(fields = {
    # The pre-bundled ES modules of each npm package, by its entry's name in the store.
    "esm": provider_field(dict[str, Artifact]),
})

_JS = (".ts", ".tsx", ".mts", ".js", ".jsx", ".mjs")
_CSS = (".css", ".scss", ".sass")

def _ext(path: str) -> str:
    name = path.split("/")[-1]
    return "." + name.split(".")[-1] if "." in name else ""

def _queries(path: str) -> list[str]:
    """The ways a source can be imported, each a module of its own. Actions are only declared:
    a module that nothing asks for is never built."""
    if path.endswith(".d.ts"):
        return []
    if path.endswith((".scss", ".sass")) and path.split("/")[-1].startswith("_"):
        # A Sass partial is only ever `@use`d by other stylesheets.
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
    virtual = {}
    for id, dep in ctx.attrs.virtual.items():
        out = dep[DefaultInfo].default_outputs[0]
        path = "{}/@virtual/{}{}".format(pkg, id.replace(":", "/"), "." + out.extension.lstrip(".") if out.extension else ".js")
        srcs[path] = out
        virtual[id] = path

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
        "css": {
            "browserslist": ctx.attrs.browserslist,
            "modules": {
                "camelCaseOnly": ctx.attrs.css_modules_camel_case_only,
                "pattern": ctx.attrs.css_modules_pattern,
            },
            "scssAdditionalData": ctx.attrs.scss_additional_data,
        },
        "define": ctx.attrs.define,
        "files": sorted(srcs.keys()),
        "imagePresets": ctx.attrs.image_presets,
        "npm": npm,
        "package": pkg,
        "virtual": virtual,
        "workspace": workspace,
    })

    def plugin_args(specs):
        if not specs:
            return None
        if ctx.attrs.plugins == None:
            fail("{}: plugins named without `plugins`, the library they are in".format(ctx.label))
        lib = ctx.attrs.plugins[NodejsLibraryInfo].direct[0]
        return cmd_args(
            # The package sits in its entry at `node_modules/<name>`, beside its dependencies.
            ["--plugins-dir", lib.entry.project("node_modules/" + lib.dir_name)] + [arg for spec in specs for arg in ["--plugin", spec]],
            hidden = lib.inputs.project_as_args("inputs"),
        )

    # Sass partials and CSS `@import`s are read from the source tree, so a stylesheet is built
    # with every stylesheet of the package.
    styles = [a for p, a in srcs.items() if p.endswith(_CSS)]
    # With `vite.persistent_worker = true`, the default in this project's `.buckconfig`, the
    # transforms run in `tools/worker.mjs`, which keeps oxc, lightningcss and Sass loaded. buck2
    # gives each command a pool of workers of its own, and a `buck2 dev` session is one command
    # that keeps its workers across its builds, so the worker starts once per session. A module's
    # transform then takes about 20ms in place of about 50ms for a new `node`. With `false`, each
    # transform starts `node`, which an executor without persistent workers needs.
    # The build plugins of each extension. A module that no plugin is for does not depend on
    # the plugins, so it neither loads them nor builds again when one changes.
    by_ext = {}
    for spec, exts in ctx.attrs.build_plugins.items():
        for ext in exts:
            by_ext.setdefault(ext, []).append(spec)
    plugins = {ext: plugin_args(specs) for ext, specs in by_ext.items()}
    base = cmd_args("--ctx", resolve_ctx)
    if read_root_config("vite", "persistent_worker", "false") == "true":
        tool = WorkerRunInfo(worker = ctx.attrs._worker[WorkerInfo], exe = ctx.attrs._transform[RunInfo].args)
    else:
        tool = ctx.attrs._transform[RunInfo]
    modules = {}
    for path, src in srcs.items():
        for query in _queries(path):
            key = path + ("~" + query if query else "")
            out = ctx.actions.declare_output("vite/m/" + key[len(pkg) + 1:] + ".js")
            ctx.actions.run(
                cmd_args(
                    base,
                    plugins.get(_ext(path), []),
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
                exe = tool,
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
            srcs = srcs,
            styles = styles,
            transform = RunInfo(args = cmd_args(ctx.attrs._transform[RunInfo], base)),
            plugins = plugins,
            server_plugins = plugin_args(ctx.attrs.server_plugins),
        ),
    ]

vite_library = rule(
    impl = _vite_library_impl,
    attrs = {
        # Import aliases, as Vite's `resolve.alias` and tsconfig's `paths`: `{"@/": "src/"}`.
        "aliases": attrs.dict(attrs.string(), attrs.string(), default = {}),
        # The browsers that stylesheets are compiled for, as `browserslist` in package.json:
        # lightningcss adds the prefixes they need, as autoprefixer does.
        "browserslist": attrs.list(attrs.string(), default = []),
        # Vite plugins of `plugins` whose build hooks (`resolveId`, `load`, `transform`) run in
        # the actions of the modules with the given extensions, as `FILE:EXPORT[:JSON args]`
        # with FILE relative to the package of `plugins`: `{"plugins/md.ts:markdown": [".md"]}`.
        # A plugin here must be a function of the module's files and nothing else.
        "build_plugins": attrs.dict(attrs.string(), attrs.list(attrs.string()), default = {}),
        # CSS modules' class names, as lightningcss patterns, and whether only the camelCase
        # form of each is exported (Vite's `css.modules.localsConvention = "camelCaseOnly"`).
        "css_modules_camel_case_only": attrs.bool(default = False),
        "css_modules_pattern": attrs.string(default = "[name]__[local]__[hash]"),
        # As Vite's `define`.
        "define": attrs.dict(attrs.string(), attrs.string(), default = {}),
        # Named sets of image directives, used as `?preset=NAME`, as vite-imagetools'
        # `defaultDirectives` would.
        "image_presets": attrs.dict(attrs.string(), attrs.dict(attrs.string(), attrs.string()), default = {}),
        # A `nodejs_library` with the app's own Vite plugins and their npm dependencies.
        "plugins": attrs.option(attrs.dep(), default = None),
        # Prepended to every Sass stylesheet, as Vite's `css.preprocessorOptions.scss.additionalData`.
        "scss_additional_data": attrs.string(default = ""),
        # Vite plugins of `plugins` whose server hooks (`configureServer`, `configResolved`,
        # `transformIndexHtml`, `hotUpdate`) the dev server runs.
        "server_plugins": attrs.list(attrs.string(), default = []),
        # Modules that Vite plugins would make up, by the ID they are imported as, such as
        # `virtual:build-info`: here they are built by targets like any other file.
        "virtual": attrs.dict(attrs.string(), attrs.dep(), default = {}),
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
        "_worker": attrs.default_only(attrs.exec_dep(default = "root//vite/tools:transform_worker")),
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
            cmd_args(tool, "--pkg", entry.project("node_modules/" + store.names[snapshot]), "--entry", name, "--out", out.as_output()),
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

def _vite_worker_impl(ctx: AnalysisContext) -> list[Provider]:
    return [DefaultInfo(), WorkerInfo(exe = ctx.attrs.worker[RunInfo].args, concurrency = None)]

# A persistent worker that runs `worker`, a program that serves buck2's worker protocol.
vite_worker = rule(
    impl = _vite_worker_impl,
    attrs = {"worker": attrs.dep(providers = [RunInfo])},
)

def _vite_constants_impl(ctx: AnalysisContext) -> list[Provider]:
    values = json.decode(ctx.attrs.values)
    lines = ["export const {} = {};".format(k, json.encode(v)) for k, v in sorted(values.items())]
    lines.append("export default {{ {} }};".format(", ".join(sorted(values.keys()))))
    out = ctx.actions.write(ctx.label.name + ".js", lines)
    return [DefaultInfo(default_output = out)]

_vite_constants = rule(
    impl = _vite_constants_impl,
    attrs = {"values": attrs.string()},
)

def vite_constants(name, values, **kwargs):
    """A module that exports each of `values`, a dict of JSON values, by its name."""
    _vite_constants(name = name, values = json.encode(values), **kwargs)

vite_ext = _ext
