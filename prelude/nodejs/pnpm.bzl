# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

"""A pnpm workspace in buck2, read straight from its `pnpm-lock.yaml`.

The lockfile is loaded as data, so there is no generated BUCK file to keep up to date: after
`pnpm add` or `pnpm install --lockfile-only`, the next build sees the change. Three pieces:

- The workspace root's `PACKAGE` file records the workspace for the packages below it:

  ```python
  load("@prelude//nodejs:pnpm.bzl", "pnpm_workspace_package")
  load(":pnpm-lock.yaml", lock = "value")
  pnpm_workspace_package(lock)
  ```

- Its `BUCK` file defines the npm store and, for each workspace package (pnpm calls them
  importers), the npm packages that package depends on. `pnpm-workspace.yaml` brings the
  patches and the packages allowed to run install scripts:

  ```python
  load("@prelude//nodejs:pnpm.bzl", "pnpm_workspace")
  load(":pnpm-lock.yaml", lock = "value")
  load(":pnpm-workspace.yaml", workspace = "value")
  pnpm_workspace(lock, workspace)
  ```

- Each workspace package's `BUCK` file defines its library, with its dependencies, workspace
  and npm alike, taken from the lockfile by the package's directory:

  ```python
  load("@prelude//nodejs:pnpm.bzl", "pnpm_package")
  pnpm_package(srcs = glob(["**/*.js"]))
  ```

A workspace package's target is named after its directory, so `packages/ui` is
`//packages/ui:ui`; that is how the packages that depend on it find it.

Bins work as under `pnpm run`. A binary or test runs with the bins of its library's
dependencies and of its top-level packages on PATH, and each library has them in
`node_modules/.bin` beside it. `//:npm/packages/ui[vite]` runs the bin npx would run for
`vite`, a dependency of `packages/ui`, and `nodejs_npm_bin` runs one by name, with `buck2 run`
or as the tool of a build action:

```python
nodejs_npm_bin(name = "tsc", links = "//:npm/packages/ui", package = "typescript", bin = "tsc")
```
"""

load(":rules.bzl", _rules = "rules")

_VALUE = "nodejs.pnpm"

# `pnpm_package` has a `package_name` parameter, like `nodejs_library`.
_current_package = package_name

# npm's `os` and `cpu` values, and the constraints they select on.
_OS_SETTINGS = {
    "darwin": "prelude//os:macos",
    "freebsd": "prelude//os:freebsd",
    "linux": "prelude//os:linux",
    "win32": "prelude//os:windows",
}

_CPU_SETTINGS = {
    "arm": "prelude//cpu:arm32",
    "arm64": "prelude//cpu:arm64",
    "ia32": "prelude//cpu:x86_32",
    "riscv64": "prelude//cpu:riscv64",
    "x64": "prelude//cpu:x86_64",
}

def _normpath(path: str) -> str:
    parts = []
    for part in path.split("/"):
        if part in ("", "."):
            continue
        if part == "..":
            if not parts:
                fail("path `{}` leaves the workspace".format(path))
            parts.pop()
        else:
            parts.append(part)
    return "/".join(parts) or "."

def _join(a: str, b: str) -> str:
    if a in ("", "."):
        return _normpath(b)
    return _normpath(a + "/" + b)

def _basename(path: str) -> str:
    return path.split("/")[-1]

def _check_lock(lock: dict) -> None:
    version = str(lock.get("lockfileVersion", ""))
    if not version.startswith("9."):
        fail("pnpm-lock.yaml: lockfileVersion `{}` is not supported; this reads version 9 (pnpm 9 and 10)".format(version))

def _snapshot_key(alias: str, version: str) -> str:
    """The snapshot key a dependency's `version` names: `name@version`, or, for an aliased
    dependency (`"alias": "npm:other@1.0.0"`), `other@1.0.0`, which the lockfile writes out."""
    if "@" in version.partition("(")[0][1:]:
        return version
    return alias + "@" + version

def _package_key(snapshot: str) -> str:
    return snapshot.partition("(")[0]

def _package_name(package: str) -> str:
    return package[: package.rindex("@")]

def _tarball_url(registry: str, package: str) -> str:
    name = _package_name(package)
    version = package[package.rindex("@") + 1 :]
    return "{}/{}/-/{}-{}.tgz".format(registry.rstrip("/"), name, _basename(name), version)

def _matches(value: str, allowed: list[str]) -> bool:
    if "!" + value in allowed:
        return False
    positive = [a for a in allowed if not a.startswith("!")]
    return not positive or value in positive

def _platform_excludes(packages: dict, settings: dict[str, str], field: str):
    """A `select` of the packages whose `field` (`os` or `cpu`) rules out each platform."""
    restricted = {key: info[field] for key, info in packages.items() if info.get(field)}
    if not restricted:
        return []
    branches = {"DEFAULT": sorted(restricted.keys())}
    for value, setting in settings.items():
        branches[setting] = sorted([key for key, allowed in restricted.items() if not _matches(value, allowed)])
    return select(branches)

def _importer_target(name: str, importer: str) -> str:
    return "{}/{}".format(name, "_" if importer == "." else importer)

def _importers(lock: dict) -> dict[str, struct]:
    """Each importer's workspace dependencies (`{alias: path}`) and npm dependencies (`{alias: snapshot}`)."""
    result = {}
    for path, importer in lock.get("importers", {}).items():
        path = _normpath(path)
        workspace = {}
        npm = {}
        optional = []
        for section in ["dependencies", "devDependencies", "optionalDependencies"]:
            for alias, dep in (importer.get(section) or {}).items():
                version = dep["version"]
                if version.startswith("link:"):
                    workspace[alias] = _join(path, version[len("link:"):])
                elif version.startswith("file:"):
                    fail("pnpm-lock.yaml: importer `{}` depends on `{}` through `{}`, which is not supported".format(path, alias, version))
                else:
                    npm[alias] = _snapshot_key(alias, version)
                    if section == "optionalDependencies":
                        optional.append(npm[alias])
        result[path] = struct(workspace = workspace, npm = npm, optional = optional)
    return result

def _settings(workspace: dict | None, root_package: dict | None) -> dict:
    """pnpm's settings: those of `pnpm-workspace.yaml` over those of the root `package.json`'s
    `pnpm` field."""
    settings = {}
    if root_package != None:
        settings.update(root_package.get("pnpm") or {})
    if workspace != None:
        settings.update(workspace)
    return settings

def _patch_files(lock: dict, settings: dict) -> dict[str, str]:
    """The patch file of each patch hash in the lockfile. pnpm 9 records the path in the
    lockfile; later versions record only the hash, and the path is in the settings."""
    configured = settings.get("patchedDependencies") or {}
    files = {}
    for key, value in (lock.get("patchedDependencies") or {}).items():
        if type(value) == "dict":
            patch_hash = value.get("hash")
            path = value.get("path")
        else:
            patch_hash = value
            path = configured.get(key)
        if not path:
            fail("pnpm-lock.yaml: `{}` is patched, but no `patchedDependencies` setting names its patch file; pass `pnpm_workspace` the loaded `pnpm-workspace.yaml` as `workspace`".format(key))
        files[str(patch_hash)] = _normpath(path)
    return files

def _patch_hash(snapshot: str) -> str | None:
    marker = "(patch_hash="
    start = snapshot.find(marker)
    if start < 0:
        return None
    start += len(marker)
    return snapshot[start:snapshot.index(")", start)]

def _spec_matches(spec: str, package: str) -> bool:
    """Whether a package key matches an entry of `allowBuilds` or `onlyBuiltDependencies`:
    a name, or a name and exact versions, as in `esbuild@0.25.0 || 0.25.1`."""
    name = _package_name(package)
    if spec == name:
        return True
    at = spec.rfind("@")
    if at <= 0 or spec[:at] != name:
        return False
    version = package[len(name) + 1:]
    return version in [v.strip() for v in spec[at + 1:].split("||")]

def _builds(settings: dict, package: str) -> bool:
    """Whether pnpm runs the install scripts of `package`. As in pnpm 10 and later, nothing runs
    them unless a setting allows it."""
    if settings.get("onlyBuiltDependenciesFile"):
        fail("pnpm settings: `onlyBuiltDependenciesFile` is not supported; list the packages in `allowBuilds` or `onlyBuiltDependencies`")
    for spec in (settings.get("neverBuiltDependencies") or []) + (settings.get("ignoredBuiltDependencies") or []):
        if _spec_matches(spec, package):
            return False
    # A matching `false` wins over a matching `true`.
    verdicts = []
    for spec, allow in (settings.get("allowBuilds") or {}).items():
        if _spec_matches(spec, package):
            if type(allow) != "bool":
                fail("pnpm settings: `allowBuilds` entry `{}` is `{}`, not true or false; run `pnpm approve-builds`".format(spec, allow))
            verdicts.append(allow)
    if verdicts:
        return False not in verdicts
    for spec in settings.get("onlyBuiltDependencies") or []:
        if _spec_matches(spec, package):
            return True
    return settings.get("dangerouslyAllowAllBuilds") == True

def pnpm_workspace(
        lock: dict,
        workspace: dict | None = None,
        root_package: dict | None = None,
        name: str = "npm",
        node_gyp: str = "node-gyp",
        build_local_only: bool = True,
        registry: str = "https://registry.npmjs.org",
        libc: str = "glibc",
        os_settings: dict[str, str] = _OS_SETTINGS,
        cpu_settings: dict[str, str] = _CPU_SETTINGS,
        visibility: list[str] = ["PUBLIC"]):
    """Defines a `nodejs_npm_store` named `name` with the npm packages of the lockfile `lock`,
    and a `nodejs_npm_links` named `<name>/<path>` with the npm dependencies of each importer at
    `path` that has any (`<name>/_` for the workspace root).

    `workspace` is the loaded `pnpm-workspace.yaml` and `root_package` the loaded root
    `package.json`; pnpm's settings come from the first, then the `pnpm` field of the second.
    Two settings matter here. `patchedDependencies` names the patch files the lockfile's
    `patchedDependencies` hashes stand for; each patched package is extracted, then patched,
    and a patch file whose sha256 differs from the lockfile's hash fails the build. Then
    `allowBuilds` (pnpm 11), or `onlyBuiltDependencies` (pnpm 10), less
    `neverBuiltDependencies` and `ignoredBuiltDependencies`, or `dangerouslyAllowAllBuilds`,
    decide which packages run their install scripts (`preinstall`, `install`, `postinstall`,
    or `node-gyp rebuild` when they have a `binding.gyp` and neither of the first two). Those
    run as build actions, with the package's dependencies beside it and their bins on PATH,
    and only locally unless `build_local_only` is False. `node-gyp` is on their PATH when the
    workspace root depends on the npm package named `node_gyp`.

    Packages restricted to other platforms by their `os` or `cpu` are left out through a
    `select` on `os_settings` and `cpu_settings`, and those restricted to a C library other
    than `libc` always are. Tarballs come from `registry` unless the lockfile gives a URL.
    """
    _check_lock(lock)
    packages = lock.get("packages") or {}
    snapshots = lock.get("snapshots") or {}
    settings = _settings(workspace, root_package)
    patch_files = _patch_files(lock, settings)
    importers = _importers(lock)

    store_packages = {}
    excluded = []
    for key, info in packages.items():
        resolution = info.get("resolution") or {}
        integrity = resolution.get("integrity")
        if not integrity:
            fail("pnpm-lock.yaml: package `{}` has no `integrity` ({}); only packages with an integrity are supported".format(key, resolution))
        store_packages[key] = (resolution.get("tarball") or _tarball_url(registry, key), integrity)
        if info.get("libc") and not _matches(libc, info["libc"]):
            excluded.append(key)

    snapshot_packages = {}
    snapshot_deps = {}
    for snapshot, info in snapshots.items():
        package = _package_key(snapshot)
        if package not in packages:
            fail("pnpm-lock.yaml: snapshot `{}` is of package `{}`, which `packages` does not list".format(snapshot, package))
        snapshot_packages[snapshot] = package
        deps = {}
        info = info or {}
        for section in ["dependencies", "optionalDependencies"]:
            for alias, version in (info.get(section) or {}).items():
                if version.startswith("link:"):
                    fail("pnpm-lock.yaml: snapshot `{}` links `{}` to `{}`, which is not supported".format(snapshot, alias, version))
                deps[alias] = _snapshot_key(alias, version)
        if deps:
            snapshot_deps[snapshot] = deps

    patches = {}
    build = []
    for snapshot, package in snapshot_packages.items():
        patch_hash = _patch_hash(snapshot)
        if patch_hash != None:
            if patch_hash not in patch_files:
                fail("pnpm-lock.yaml: snapshot `{}` is patched, but `patchedDependencies` has no patch with that hash".format(snapshot))
            patches[snapshot] = (patch_files[patch_hash], patch_hash)
        if _builds(settings, package):
            build.append(snapshot)
    root = importers.get(".")
    node_gyp_snapshot = root.npm.get(node_gyp) if root != None else None

    _rules["nodejs_npm_store"](
        name = name,
        packages = store_packages,
        snapshot_packages = snapshot_packages,
        snapshots = snapshot_deps,
        exclude = excluded + _platform_excludes(packages, os_settings, "os") + _platform_excludes(packages, cpu_settings, "cpu"),
        patches = patches,
        build = build,
        node_gyp = node_gyp_snapshot,
        build_local_only = build_local_only,
        visibility = visibility,
    )

    for path, importer in importers.items():
        if importer.npm:
            _rules["nodejs_npm_links"](
                name = _importer_target(name, path),
                store = ":" + name,
                packages = importer.npm,
                optional = importer.optional,
                bins = [alias for alias, snapshot in importer.npm.items() if (packages.get(_package_key(snapshot)) or {}).get("hasBin")],
                visibility = visibility,
            )

def pnpm_workspace_package(lock: dict, name: str = "npm"):
    """Records the workspace of `lock` for the packages below this `PACKAGE` file, for
    `pnpm_package` and `pnpm_importer`. `name` is the one given to `pnpm_workspace`."""
    _check_lock(lock)
    importers = {}
    names = {}
    for path, importer in _importers(lock).items():
        importers[path] = {"npm": bool(importer.npm), "workspace": importer.workspace}
        for alias, dep in importer.workspace.items():
            names[dep] = alias
    write_package_value(_VALUE, {
        "importers": importers,
        "names": names,
        "path": package_name(),
        "store": name,
    })

def pnpm_importer() -> struct:
    """This package as a pnpm workspace package (an importer): `deps`, the targets of the
    workspace packages it depends on, `node_modules`, the target linking its npm dependencies,
    `name`, its target name (its directory name), and `package_name`, the name the packages that
    depend on it use, or `None` when none does."""
    workspace = read_package_value(_VALUE)
    if workspace == None:
        fail("{}: no pnpm workspace above this package; its root's PACKAGE file calls `pnpm_workspace_package`".format(package_name()))
    root = workspace["path"]
    here = package_name()
    if root and here != root and not here.startswith(root + "/"):
        fail("{}: not inside the pnpm workspace at `//{}`".format(here, root))
    path = _normpath(here[len(root):]) if root else _normpath(here)
    importer = workspace["importers"].get(path)
    if importer == None:
        fail("{}: `{}` is not a package in the pnpm workspace at `//{}`; list it in pnpm-workspace.yaml and run `pnpm install`".format(here, path, root))
    deps = []
    for dep in importer["workspace"].values():
        deps.append("//{}:{}".format(_join(root, dep), _basename(_join(root, dep))))
    node_modules = ["//{}:{}".format(root, _importer_target(workspace["store"], path))] if importer["npm"] else []
    return struct(
        deps = deps,
        node_modules = node_modules,
        name = _basename(here),
        package_name = workspace["names"].get(path),
    )

def pnpm_package(
        package_name: str | None = None,
        deps: list = [],
        node_modules: list = [],
        visibility: list[str] = ["PUBLIC"],
        **kwargs):
    """A `nodejs_library` for this pnpm workspace package, named after its directory, with its
    `package.json` and with the dependencies the lockfile gives it, workspace and npm, added to
    `deps` and `node_modules`. `package_name` defaults to the name its dependents use for it; a
    package nothing in the workspace depends on needs it set."""
    importer = pnpm_importer()
    package_name = package_name or importer.package_name
    if package_name == None:
        fail("{}: nothing in the pnpm workspace depends on this package, so set `package_name` (the `name` in its package.json)".format(_current_package()))
    _rules["nodejs_library"](
        name = importer.name,
        package_name = package_name,
        package_json = "package.json",
        deps = importer.deps + deps,
        node_modules = importer.node_modules + node_modules,
        visibility = visibility,
        **kwargs
    )
