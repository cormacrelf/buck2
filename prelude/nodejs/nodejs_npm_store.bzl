# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

load("@prelude//:command_alias.bzl", "command_alias", "command_alias_providers")
load("@prelude//:prelude_rule.bzl", "prelude_rule")
load("@prelude//decls:common.bzl", "buck")
load("@prelude//decls:toolchains_common.bzl", "toolchains_common")
load("@prelude//os_lookup:defs.bzl", "Os", "OsLookup")
load(":nodejs_closure.bzl", "npm_bins_dir")
load(":nodejs_providers.bzl", "NodejsInputsTSet", "NodejsLinkedPackage", "NodejsLinksInfo", "package_inputs")
load(":nodejs_toolchain.bzl", "NodejsToolchainInfo")

NodejsNpmStoreInfo = provider(
    doc = "The entries of a `nodejs_npm_store`, by snapshot key, such as `react-dom@19.1.1(react@19.1.1)`.",
    fields = {
        # Each snapshot's entry: `node_modules/<name>`, the package, beside its dependencies.
        "entries": provider_field(dict[str, Artifact]),
        # Each snapshot's package name.
        "names": provider_field(dict[str, str]),
        # Each snapshot's entry and those its symlinks lead to, transitively.
        "closures": provider_field(dict[str, NodejsInputsTSet]),
    },
)

def npm_store_name(snapshot: str) -> str:
    """The directory name of a snapshot's entry in the store: pnpm's encoding, roughly."""
    name = snapshot.replace("/", "+").replace("(", "_").replace(")", "").replace(":", "+")
    if len(name) > 120:
        name = name[:100] + "_" + str(hash(snapshot) & 0xffffffff)
    return name

def _package_name(package: str) -> str:
    return package[: package.rindex("@")] if package.rindex("@") > 0 else package

def _link_target(alias: str, dep: str, dep_name: str) -> str:
    # From `<entry>/node_modules/<alias>` (one level deeper for a scoped alias) to the dep's
    # package directory in its own entry, a sibling of this one under `store/`.
    up = "../../../" if alias.startswith("@") else "../../"
    return up + npm_store_name(dep) + "/node_modules/" + dep_name

def strongly_connected_components(nodes: list[str], edges: dict[str, list[str]]) -> list[list[str]]:
    """The strongly connected components of a graph, each listed after every component it
    reaches (Tarjan's algorithm, iterative: Starlark has no recursion)."""
    index = {}
    low = {}
    on_stack = {}
    stack = []
    components = []
    steps = len(nodes) + 1
    for n in nodes:
        steps += len(edges.get(n, []))
    for root in nodes:
        if root in index:
            continue
        index[root] = len(index)
        low[root] = index[root]
        stack.append(root)
        on_stack[root] = True
        work = [(root, 0)]
        for _ in range(steps):
            if not work:
                break
            node, i = work[-1]
            successors = edges.get(node, [])
            if i < len(successors):
                work[-1] = (node, i + 1)
                succ = successors[i]
                if succ not in index:
                    index[succ] = len(index)
                    low[succ] = index[succ]
                    stack.append(succ)
                    on_stack[succ] = True
                    work.append((succ, 0))
                elif on_stack.get(succ):
                    low[node] = min(low[node], index[succ])
                continue
            work.pop()
            if work:
                parent = work[-1][0]
                low[parent] = min(low[parent], low[node])
            if low[node] == index[node]:
                component = []
                for _ in range(len(stack)):
                    member = stack.pop()
                    on_stack[member] = False
                    component.append(member)
                    if member == node:
                        break
                components.append(component)
    return components

def _entry_action(
        ctx: AnalysisContext,
        snapshot: str,
        tarball: Artifact,
        name: str,
        links: list,
        entry: Artifact,
        patch: (Artifact, str) | None,
        build: cmd_args | None) -> None:
    links_file = ctx.actions.write_json("links/" + npm_store_name(snapshot) + ".json", links, has_content_based_path = False)
    cmd = cmd_args(
        ctx.attrs._package_tree_tool[RunInfo],
        "npm-entry",
        "--tarball",
        tarball,
        "--name",
        name,
        "--links",
        links_file,
        "--out",
        entry.as_output(),
    )
    if patch != None:
        cmd.add("--patch", patch[0], "--patch-hash", patch[1])
    if build != None:
        cmd.add(build)
    ctx.actions.run(
        cmd,
        category = "npm_entry" if build == None else "npm_install_scripts",
        identifier = snapshot,
        local_only = build != None and ctx.attrs.build_local_only,
    )

def _script_args(
        ctx: AnalysisContext,
        toolchain: NodejsToolchainInfo,
        snapshot: str,
        component: list[str],
        below: list[NodejsInputsTSet],
        build: dict[str, bool],
        entries: dict[str, Artifact],
        names: dict[str, str],
        node_gyp: str | None,
        component_of: dict[str, int],
        tsets: list[NodejsInputsTSet]) -> cmd_args:
    """The arguments of `npm-entry` that run the install scripts of `snapshot`, with the entries
    its links lead to as inputs: the closures below its component, and the entries of the other
    members of its component that run no install scripts (a package in a dependency cycle with
    another that runs them does not see it while its own run)."""
    if toolchain.node_exec == None:
        fail("{}: `{}` runs install scripts, which need `node_exec` on the nodejs toolchain".format(ctx.label, snapshot))
    args = cmd_args("--build", "--node", toolchain.node_exec)
    children = list(below)
    if node_gyp != None and node_gyp in component_of:
        children.append(tsets[component_of[node_gyp]])
        args.add("--node-gyp", entries[node_gyp].project("node_modules/" + names[node_gyp]))
    value = [entries[s] for s in sorted(component) if s not in build]
    kwargs = {"children": children} if children else {}
    if value:
        kwargs["value"] = value
    args.add(cmd_args(hidden = ctx.actions.tset(NodejsInputsTSet, **kwargs).project_as_args("inputs")))
    return args

def _nodejs_npm_store_impl(ctx: AnalysisContext) -> list[Provider]:
    excluded = {p: True for p in ctx.attrs.exclude}
    tarballs = {}
    for package, (url, sri) in ctx.attrs.packages.items():
        if package in excluded:
            continue
        tarballs[package] = ctx.actions.download_file(
            "tarballs/" + npm_store_name(package) + ".tgz",
            url,
            has_content_based_path = False,
            # Passed through a dict so the module still loads under a buck2 that predates `sri`,
            # such as the bootstrap binary that builds buck2.
            **{"sri": sri}
        )

    snapshot_packages = {}
    for snapshot, package in ctx.attrs.snapshot_packages.items():
        if package not in ctx.attrs.packages:
            fail("{}: snapshot `{}` is of package `{}`, which is not in `packages`".format(ctx.label, snapshot, package))
        if package not in excluded:
            snapshot_packages[snapshot] = package
    for snapshot in ctx.attrs.patches:
        if snapshot not in ctx.attrs.snapshot_packages:
            fail("{}: `patches` has snapshot `{}`, which is not in `snapshot_packages`".format(ctx.label, snapshot))
    build = {}
    for snapshot in ctx.attrs.build:
        if snapshot not in ctx.attrs.snapshot_packages:
            fail("{}: `build` has snapshot `{}`, which is not in `snapshot_packages`".format(ctx.label, snapshot))
        if snapshot in snapshot_packages:
            build[snapshot] = True
    node_gyp = ctx.attrs.node_gyp
    if node_gyp != None and node_gyp not in snapshot_packages:
        fail("{}: `node_gyp` is snapshot `{}`, which is not in `snapshot_packages`".format(ctx.label, node_gyp))

    # Every entry is declared first: the closures hold them, and a package with install scripts
    # runs them with the closures of its dependencies as inputs.
    entries = {}
    names = {}
    links = {}
    deps = {}
    for snapshot, package in snapshot_packages.items():
        name = _package_name(package)
        snapshot_links = []
        snapshot_deps = []
        for alias, dep in ctx.attrs.snapshots.get(snapshot, {}).items():
            if dep not in snapshot_packages:
                # A platform-specific optional dependency that is excluded on this platform.
                if dep in ctx.attrs.snapshot_packages:
                    continue
                fail("{}: snapshot `{}` depends on `{}`, which is not in `snapshot_packages`".format(ctx.label, snapshot, dep))
            if alias == name:
                # A package cannot sit beside itself; a dependency on itself resolves to itself.
                continue
            snapshot_links.append((alias, _link_target(alias, dep, _package_name(snapshot_packages[dep]))))
            snapshot_deps.append(dep)
        entries[snapshot] = ctx.actions.declare_output("store/" + npm_store_name(snapshot), dir = True, has_content_based_path = False)
        names[snapshot] = name
        links[snapshot] = snapshot_links
        deps[snapshot] = snapshot_deps

    # Entries without install scripts need only their tarballs.
    for snapshot, package in snapshot_packages.items():
        if snapshot not in build:
            _entry_action(ctx, snapshot, tarballs[package], names[snapshot], links[snapshot], entries[snapshot], ctx.attrs.patches.get(snapshot), None)

    # One tset node per strongly connected component, each after the components it depends on,
    # whose entries are bound by then: an entry that runs install scripts takes the closures of
    # the components below its own as inputs. node-gyp goes first, so that its closure is
    # there for every package that runs scripts.
    toolchain = ctx.attrs._nodejs_toolchain[NodejsToolchainInfo]
    roots = ([node_gyp] if node_gyp != None else []) + [s for s in entries if s != node_gyp]
    tsets = []
    component_of = {}
    for component in strongly_connected_components(roots, deps):
        children = {}
        for s in component:
            for dep in deps[s]:
                if dep not in component:
                    children[component_of[dep]] = True
        below = [tsets[c] for c in children]
        for snapshot in sorted(component):
            if snapshot in build:
                _entry_action(
                    ctx,
                    snapshot,
                    tarballs[snapshot_packages[snapshot]],
                    names[snapshot],
                    links[snapshot],
                    entries[snapshot],
                    ctx.attrs.patches.get(snapshot),
                    _script_args(ctx, toolchain, snapshot, component, below, build, entries, names, node_gyp, component_of, tsets),
                )
        kwargs = {"children": below} if below else {}
        tsets.append(ctx.actions.tset(NodejsInputsTSet, value = [entries[s] for s in sorted(component)], **kwargs))
        for s in component:
            component_of[s] = len(tsets) - 1
    closures = {s: tsets[c] for s, c in component_of.items()}

    return [
        DefaultInfo(sub_targets = {npm_store_name(s): [DefaultInfo(default_output = e)] for s, e in entries.items()}),
        NodejsNpmStoreInfo(entries = entries, names = names, closures = closures),
    ]

nodejs_npm_store = prelude_rule(
    name = "nodejs_npm_store",
    impl = _nodejs_npm_store_impl,
    docs = """Third-party npm packages in a pnpm-style store, usually made by
    `pnpm_workspace` from a `pnpm-lock.yaml`. `packages` maps each package
    key, `name@version`, to its tarball URL and its integrity, a
    Subresource Integrity string such as the lockfile's `sha512-...`.
    `snapshot_packages` maps each snapshot key (a package key, with the
    versions of its peer dependencies in parentheses when they decide
    which package it links to, as in `react-dom@19.1.1(react@19.1.1)`) to
    its package, and `snapshots` maps a snapshot key to its dependencies:
    the name each is linked under, mapped to the snapshot key it links to.
    `exclude` lists package keys that are left out, together with the
    links to them: typically, through a `select`, the platform-specific
    optional dependencies that do not match the target platform.

    Each snapshot is one entry, built only when something needs it: the
    tarball is downloaded and checked against its integrity, then
    extracted to `node_modules/<name>` inside the entry, and each
    dependency is a relative symlink beside it to the dependency's entry,
    as in pnpm's `node_modules/.pnpm`. Node resolves a package from its
    real path, so a package finds exactly its declared dependencies.
    Entries refer to each other only by path, so dependency cycles, which
    npm allows, need nothing special. `nodejs_npm_links` targets link the
    store's packages into the `node_modules` of other targets. Each entry
    is a sub-target, named by its directory name in the store.""",
    examples = None,
    further = None,
    attrs = (
        # @unsorted-dict-items
        {
            "packages": attrs.dict(attrs.string(), attrs.tuple(attrs.string(), attrs.string()), sorted = False, default = {}),
            "snapshot_packages": attrs.dict(attrs.string(), attrs.string(), sorted = False, default = {}),
            "snapshots": attrs.dict(attrs.string(), attrs.dict(attrs.string(), attrs.string(), sorted = False), sorted = False, default = {}),
            "exclude": attrs.list(attrs.string(), default = []),
            "patches": attrs.dict(attrs.string(), attrs.tuple(attrs.source(), attrs.string()), sorted = False, default = {}),
            "build": attrs.list(attrs.string(), default = []),
            "node_gyp": attrs.option(attrs.string(), default = None),
            "build_local_only": attrs.bool(default = True),
            "_nodejs_toolchain": toolchains_common.nodejs(),
            "_package_tree_tool": attrs.default_only(attrs.exec_dep(providers = [RunInfo], default = "prelude//nodejs/tools:package_tree")),
        }
    ),
)

def bin_providers(ctx: AnalysisContext, bins: Artifact, inputs: NodejsInputsTSet, alias: str, bin: str | None) -> list[Provider]:
    """`DefaultInfo` and `RunInfo` running the bin `bin`, or `alias`'s default bin (the one npx
    runs), from `bins`, a directory of `npm_bins_dir`, with its `.bin` on PATH."""
    path = "node_modules/.bin/" + (bin if bin != None else ".default/" + alias)
    output = command_alias(
        actions = ctx.actions,
        path = "__bins__/" + (bin if bin != None else alias),
        target_os = ctx.attrs._target_os_type[OsLookup],
        base = ctx.attrs._nodejs_toolchain[NodejsToolchainInfo].node,
        args = cmd_args("--require", ctx.attrs._path_preload, bins.project(path), hidden = inputs.project_as_args("inputs")),
        env = {"NODEJS_PREPEND_PATH": bins.project("node_modules/.bin")},
        labels = [],
    )
    return command_alias_providers(output, [bins])

_BIN_ATTRS = {
    "_nodejs_toolchain": toolchains_common.nodejs(),
    "_package_tree_tool": attrs.default_only(attrs.exec_dep(providers = [RunInfo], default = "prelude//nodejs/tools:package_tree")),
    "_path_preload": attrs.default_only(attrs.source(default = "prelude//nodejs/tools:path_preload.cjs")),
    "_target_os_type": buck.target_os_type_arg(),
}

def _nodejs_npm_links_impl(ctx: AnalysisContext) -> list[Provider]:
    store = ctx.attrs.store[NodejsNpmStoreInfo]
    links = []
    for alias, snapshot in ctx.attrs.packages.items():
        if snapshot not in store.entries:
            if snapshot in ctx.attrs.optional:
                continue
            fail("{}: `{}` links snapshot `{}`, which `{}` does not have".format(ctx.label, alias, snapshot, ctx.attrs.store.label.raw_target()))
        name = store.names[snapshot]
        links.append(NodejsLinkedPackage(
            name = alias,
            dir = store.entries[snapshot].project("node_modules/" + name),
            closure = store.closures[snapshot],
            owner = ctx.label,
        ))
    bins = npm_bins_dir(
        ctx.actions,
        ctx.attrs._package_tree_tool[RunInfo],
        "bins",
        {link.name: link.dir for link in links},
        ctx.attrs._target_os_type[OsLookup].os == Os("windows"),
    )
    inputs = package_inputs(ctx.actions, None, [], links)
    sub_targets = {"bin": [DefaultInfo(default_output = bins.project("node_modules/.bin"), other_outputs = [inputs.project_as_args("inputs")])]}
    linked = {link.name: True for link in links}
    for alias in ctx.attrs.bins:
        if alias not in ctx.attrs.packages:
            fail("{}: `bins` has `{}`, which is not in `packages`".format(ctx.label, alias))
        if alias in linked:
            sub_targets[alias] = bin_providers(ctx, bins, inputs, alias, None)
    return [
        DefaultInfo(default_outputs = [link.dir for link in links], sub_targets = sub_targets),
        NodejsLinksInfo(links = links, bins = bins, inputs = inputs),
    ]

nodejs_npm_links = prelude_rule(
    name = "nodejs_npm_links",
    impl = _nodejs_npm_links_impl,
    docs = """Packages of a `nodejs_npm_store`, to be linked under
    `node_modules` by name. `packages` maps the name each is linked under
    to its snapshot key in the store; `optional` lists snapshot keys the
    store may leave out (platform-specific optional dependencies). List
    the target in the `node_modules` of a `nodejs_library`, to link the
    packages next to that library's package, where only its code sees
    them, or of a `nodejs_binary` or `nodejs_test`, to link them at the
    top of its `node_modules`.

    The `[bin]` sub-target is a `node_modules/.bin` of the packages' bins,
    as npm writes it. Each of `bins`, the names of packages that declare
    bins, is also a runnable sub-target, `[<name>]`, that runs the bin npx
    runs for that package (its only bin, or the one named after it) with
    `[bin]` on PATH; `nodejs_npm_bin` runs a bin by its own name.""",
    examples = None,
    further = None,
    attrs = (
        # @unsorted-dict-items
        {
            "store": attrs.dep(providers = [NodejsNpmStoreInfo]),
            "packages": attrs.dict(attrs.string(), attrs.string(), sorted = False, default = {}),
            "optional": attrs.list(attrs.string(), default = []),
            "bins": attrs.list(attrs.string(), default = []),
        } | _BIN_ATTRS
    ),
)

def _nodejs_npm_bin_impl(ctx: AnalysisContext) -> list[Provider]:
    info = ctx.attrs.links[NodejsLinksInfo]
    if ctx.attrs.package not in [link.name for link in info.links]:
        fail("{}: `{}` does not link `{}`".format(ctx.label, ctx.attrs.links.label.raw_target(), ctx.attrs.package))
    return bin_providers(ctx, info.bins, info.inputs, ctx.attrs.package, ctx.attrs.bin)

nodejs_npm_bin = prelude_rule(
    name = "nodejs_npm_bin",
    impl = _nodejs_npm_bin_impl,
    docs = """Runs a bin of a package that a `nodejs_npm_links` target links:
    `bin`, or, when it is unset, the one npx runs for `package` (its only
    bin, or the one named after it). Its `node_modules/.bin`, the bins of
    every package `links` links, is on PATH, as under `pnpm exec`, and so
    is the directory of the Node that runs it. Use it with `buck2 run`, or
    as the tool of a build action:

    ```python
    nodejs_npm_bin(name = "tsc", links = "//:npm/packages/app", package = "typescript", bin = "tsc")
    ```""",
    examples = None,
    further = None,
    attrs = (
        # @unsorted-dict-items
        {
            "links": attrs.dep(providers = [NodejsLinksInfo]),
            "package": attrs.string(),
            "bin": attrs.option(attrs.string(), default = None),
        } | _BIN_ATTRS
    ),
)
