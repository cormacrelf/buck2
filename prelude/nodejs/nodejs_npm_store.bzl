# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

load("@prelude//:prelude_rule.bzl", "prelude_rule")
load(":nodejs_providers.bzl", "NodejsLinkedPackage", "NodejsLinksInfo")

NodejsNpmStoreInfo = provider(
    doc = "The entries of a `nodejs_npm_store`, by snapshot key, such as `react-dom@19.1.1(react@19.1.1)`.",
    fields = {
        # Each snapshot's entry: `node_modules/<name>`, the package, beside its dependencies.
        "entries": provider_field(dict[str, Artifact]),
        # Each snapshot's package name.
        "names": provider_field(dict[str, str]),
        # Each snapshot's dependencies, by snapshot key.
        "deps": provider_field(dict[str, list[str]]),
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

def _nodejs_npm_store_impl(ctx: AnalysisContext) -> list[Provider]:
    excluded = {p: True for p in ctx.attrs.exclude}
    tool = ctx.attrs._package_tree_tool[RunInfo]
    tarballs = {}
    for package, (url, sri) in ctx.attrs.packages.items():
        if package in excluded:
            continue
        tarballs[package] = ctx.actions.download_file(
            "tarballs/" + npm_store_name(package) + ".tgz",
            url,
            sri = sri,
            has_content_based_path = False,
        )

    snapshot_packages = {}
    for snapshot, package in ctx.attrs.snapshot_packages.items():
        if package not in ctx.attrs.packages:
            fail("{}: snapshot `{}` is of package `{}`, which is not in `packages`".format(ctx.label, snapshot, package))
        if package not in excluded:
            snapshot_packages[snapshot] = package

    entries = {}
    names = {}
    deps = {}
    sub_targets = {}
    for snapshot, package in snapshot_packages.items():
        name = _package_name(package)
        links = []
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
            links.append((alias, _link_target(alias, dep, _package_name(snapshot_packages[dep]))))
            snapshot_deps.append(dep)
        entry = ctx.actions.declare_output("store/" + npm_store_name(snapshot), dir = True, has_content_based_path = False)
        links_file = ctx.actions.write_json("links/" + npm_store_name(snapshot) + ".json", links, has_content_based_path = False)
        ctx.actions.run(
            cmd_args(
                tool,
                "npm-entry",
                "--tarball",
                tarballs[package],
                "--name",
                name,
                "--links",
                links_file,
                "--out",
                entry.as_output(),
            ),
            category = "npm_entry",
            identifier = snapshot,
        )
        entries[snapshot] = entry
        names[snapshot] = name
        deps[snapshot] = snapshot_deps
        sub_targets[npm_store_name(snapshot)] = [DefaultInfo(default_output = entry)]

    return [
        DefaultInfo(sub_targets = sub_targets),
        NodejsNpmStoreInfo(entries = entries, names = names, deps = deps),
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
            "_package_tree_tool": attrs.default_only(attrs.exec_dep(providers = [RunInfo], default = "prelude//nodejs/tools:package_tree")),
        }
    ),
)

def _closure(store: NodejsNpmStoreInfo, snapshot: str) -> list[Artifact]:
    seen = {snapshot: True}
    queue = [snapshot]
    for _ in range(len(store.entries) + 1):
        if not queue:
            break
        next = []
        for s in queue:
            for dep in store.deps[s]:
                if dep not in seen:
                    seen[dep] = True
                    next.append(dep)
        queue = next
    return [store.entries[s] for s in seen]

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
            closure = _closure(store, snapshot),
            owner = ctx.label,
        ))
    return [
        DefaultInfo(default_outputs = [link.dir for link in links]),
        NodejsLinksInfo(links = links),
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
    top of its `node_modules`.""",
    examples = None,
    further = None,
    attrs = (
        # @unsorted-dict-items
        {
            "store": attrs.dep(providers = [NodejsNpmStoreInfo]),
            "packages": attrs.dict(attrs.string(), attrs.string(), sorted = False, default = {}),
            "optional": attrs.list(attrs.string(), default = []),
        }
    ),
)
