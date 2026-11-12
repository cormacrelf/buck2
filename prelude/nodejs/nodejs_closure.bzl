# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

load(":nodejs_providers.bzl", "NodejsLibraryPackage", "NodejsLinkedPackage", "NodejsTreeInfo", "check_relative_path")

def _assembled_dir(actions: AnalysisActions):
    # Feature-detected, as in `content_based_dist.bzl`, so the module loads under a buck2 without it.
    entry = getattr(__buck2_builtins__, "assembled_dir", None)
    make = getattr(actions, "assembled_dir", None)
    if entry == None or make == None:
        fail("the Node.js rules need a buck2 with `ctx.actions.assembled_dir`")
    return entry, make

def _claim(claimed: dict[str, str], name: str, owner: str, consumer) -> None:
    existing = claimed.get(name)
    if existing != None and existing != owner:
        fail("{}: package '{}' is provided by both `{}` and `{}`".format(consumer, name, existing, owner))
    claimed[name] = owner

def package_entry(
    actions: AnalysisActions,
    name: str,
    label: Label,
    dir_name: str,
    package_dir: Artifact,
    direct: list[NodejsLibraryPackage],
    links: list[NodejsLinkedPackage],
    types: bool) -> Artifact:
    """A pnpm-style entry for one first-party package: `node_modules/<dir_name>`, a copy of
    `package_dir`, beside links to the entries of `direct` (`types_entry` if `types`, else
    `entry`) and to the npm store packages of `links`.

    Node and `tsc` resolve a package from its real path, so code in the copy finds exactly
    these siblings, and through `node_modules` ancestors, whatever the consumer composes above.
    """
    entry, make = _assembled_dir(actions)
    contents = {"node_modules/" + dir_name: entry.copy(package_dir)}
    claimed = {dir_name: str(label.raw_target())}
    for pkg in direct:
        if pkg.label == label:
            continue
        _claim(claimed, pkg.dir_name, str(pkg.label.raw_target()), label)
        target = pkg.types_entry if types else pkg.entry
        contents["node_modules/" + pkg.dir_name] = entry.symlink(target.project("node_modules/" + pkg.dir_name))
    for link in links:
        _claim(claimed, link.name, str(link.owner.raw_target()), label)
        contents["node_modules/" + link.name] = entry.symlink(link.dir)
    return make(name, contents = contents)

def compose_node_modules(
    actions: AnalysisActions,
    name: str,
    packages: dict[str, Artifact],
    owners: dict[str, Label],
    trees: list[NodejsTreeInfo],
    root_files: dict[str, Artifact] = {},
    root_file_desc: str = "root file '{path}'",
    links: list[NodejsLinkedPackage] = []) -> Artifact:
    """A directory holding one `node_modules` of first-party `packages`, npm store `links` and third-party `trees`.

    `packages` and `owners` are keyed by install directory; each package is a symlink to
    the package directory of its entry (`NodejsLibraryPackage.entry`), so it resolves its own
    dependencies there. `links` are symlinks into an npm store. Each tree's packages, minus its
    `exclude`, are projected out of it and copied as its manifest lists them; one listed with an
    `error` fails. A scope directory is projected whole unless another source
    provides packages in it, one of its packages is excluded, one is a
    symlink, or it holds a dot entry. Dot entries such as `.bin` are never composed. `root_files` are
    placed beside `node_modules` at their keys, which cannot be under
    `node_modules` when anything is composed; `root_file_desc` names the
    offending file in that error, formatted with `path` and `artifact`.
    """
    symlinks = {"node_modules/" + n: packages[n] for n in packages}
    first_party = {n: str(owners[n].raw_target()) for n in packages}
    for link in links:
        _claim(first_party, link.name, str(link.owner.raw_target()), name)
        symlinks["node_modules/" + link.name] = link.dir
    srcs = {}
    for root_name, root_file in root_files.items():
        segments = check_relative_path(root_name, "invalid `root_files` path", allow_colon = True)
        if (symlinks or trees) and segments[0].lower() == "node_modules":
            fail("{} collides with the composed `node_modules` tree".format(root_file_desc.format(path = root_name, artifact = root_file)))
        srcs[root_name] = root_file
    if not trees:
        return _assemble(actions, name, name, srcs, symlinks)
    out = actions.declare_output(name, dir = True)
    actions.dynamic_output_new(
        _compose(
            first_party = first_party,
            manifests = [tree.manifest for tree in trees],
            name = name,
            out = out.as_output(),
            srcs = srcs,
            symlinks = symlinks,
            trees = [(tree.node_modules, str(tree.owner.raw_target()), tree.exclude) for tree in trees],
        ),
    )
    return out

def _assemble(actions: AnalysisActions, out: str | OutputArtifact, name: str, srcs: dict[str, Artifact], symlinks: dict[str, Artifact]) -> Artifact:
    if not srcs and not symlinks:
        return actions.copied_dir(out, {"node_modules": actions.copied_dir(name + "_empty", {})})
    if not symlinks:
        return actions.copied_dir(out, srcs)
    entry, make = _assembled_dir(actions)
    contents = {path: entry.copy(artifact) for path, artifact in srcs.items()}
    for path, artifact in symlinks.items():
        contents[path] = entry.symlink(artifact)
    return make(out, contents = contents)

def _package_root(dir_name: str) -> str:
    segments = dir_name.split("/")
    return "/".join(segments[:2]) if segments[0].startswith("@") else segments[0]

def _scope(package_name: str) -> str | None:
    return package_name.split("/")[0] if package_name.startswith("@") else None

def _tree_packages(manifest: ArtifactValue, owner: str, exclude: list[str]) -> (list[dict[str, typing.Any]], dict[str, bool]):
    """The packages of one tree that are composed, and the scopes they cannot be projected whole from."""
    packages = []
    excluded = {}
    contents = manifest.read_json()
    split_scopes = {_scope(n): True for n in contents["ignored"] if _scope(n)}
    for package in contents["packages"]:
        package_name = package["name"]
        scope = _scope(package_name)
        excluded_as = [n for n in (package_name, scope) if n and n in exclude]
        if excluded_as:
            for n in excluded_as:
                excluded[n] = True
            if scope:
                split_scopes[scope] = True
            continue
        if "error" in package:
            fail("package '{}' in `{}` cannot be composed: {}".format(package_name, owner, package["error"]))
        if "link" in package:
            if "path" not in package:
                fail(
                    "package '{}' in `{}` is reached through a symlink to '{}', which {}; it cannot be composed (drop it with `exclude`)".format(
                        package_name,
                        owner,
                        package["link"],
                        "resolves to the tree itself or outside it" if package.get("outside") else "does not resolve to a file or directory",
                    )
                )
            if scope:
                split_scopes[scope] = True
        packages.append(package)
    for package_name in exclude:
        if package_name not in excluded:
            fail("`exclude` on `{}` names package '{}', which the tree does not provide".format(owner, package_name))
    return packages, split_scopes

def _compose_impl(
    actions: AnalysisActions,
    first_party: dict[str, str],
    manifests: list[ArtifactValue],
    name: str,
    out: OutputArtifact,
    srcs: dict[str, Artifact],
    symlinks: dict[str, Artifact],
    trees: list[(Artifact, str, list[str])],
) -> list[Provider]:
    claimed = dict(first_party)
    first_party_roots = {_package_root(n): owner for n, owner in first_party.items()}
    shared_scopes = {_scope(n): True for n in first_party if _scope(n)}

    tree_packages = []
    scope_owners = {}
    for (_tree, owner, exclude), manifest in zip(trees, manifests):
        packages, split_scopes = _tree_packages(manifest, owner, exclude)
        shared_scopes.update(split_scopes)
        for package in packages:
            scope = _scope(package["name"])
            if scope:
                scope_owners.setdefault(scope, {})[owner] = True
        tree_packages.append(packages)
    for scope, owners in scope_owners.items():
        if len(owners) > 1:
            shared_scopes[scope] = True

    srcs = dict(srcs)
    claimed_by_tree = {}
    for i, ((tree, owner, _exclude), packages) in enumerate(zip(trees, tree_packages)):
        for package in packages:
            package_name = package["name"]
            scope = _scope(package_name)
            key = scope if scope and scope not in shared_scopes else package_name
            if claimed_by_tree.get(key) == i:
                continue
            existing = claimed.get(key) or first_party_roots.get(key) or (scope and first_party_roots.get(scope))
            if existing != None:
                fail("package '{}' is provided by both `{}` and `{}`; drop one with `exclude` on the `nodejs_node_modules`".format(key, existing, owner))
            claimed[key] = owner
            claimed_by_tree[key] = i
            srcs["node_modules/" + key] = tree.project(package.get("path", key))
    _assemble(actions, out, name, srcs, symlinks)
    return []

_compose = dynamic_actions(
    impl = _compose_impl,
    attrs = {
        "first_party": dynattrs.value(dict[str, str]),
        "manifests": dynattrs.list(dynattrs.artifact_value()),
        "name": dynattrs.value(str),
        "out": dynattrs.output(),
        "srcs": dynattrs.value(dict[str, Artifact]),
        "symlinks": dynattrs.value(dict[str, Artifact]),
        "trees": dynattrs.value(list[(Artifact, str, list[str])]),
    },
)
