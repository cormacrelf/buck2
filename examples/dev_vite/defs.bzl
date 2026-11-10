# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# Rules for a Vite app that `buck2 dev` runs with hot module replacement.
#
# These rules are a quick way to get there, not a model for real JavaScript builds:
# `pnpm_install` runs `pnpm install` with network access inside a local action, using the
# `pnpm` on your `PATH`.

def _pnpm_install_impl(ctx):
    srcs = ctx.actions.symlinked_dir("srcs", {s.basename: s for s in ctx.attrs.srcs})
    script = ctx.actions.write(
        "install.sh",
        cmd_args(
            "#!/usr/bin/env bash",
            "set -euo pipefail",
            'OUT="$1"',
            cmd_args("SRCS", srcs, delimiter = "="),
            # pnpm writes next to package.json, so install in a copy. The paths are relative
            # to the project root, so `cd` only in a subshell.
            'cp -RL "$SRCS" "$BUCK_SCRATCH_PATH/pkg"',
            '(cd "$BUCK_SCRATCH_PATH/pkg" && pnpm install --frozen-lockfile)',
            'mv "$BUCK_SCRATCH_PATH/pkg" "$OUT"',
            delimiter = "\n",
        ),
        is_executable = True,
        with_inputs = True,
    )
    out = ctx.actions.declare_output("installed", dir = True)
    ctx.actions.run(
        cmd_args(script, out.as_output()),
        category = "pnpm_install",
        local_only = True,
    )
    return [DefaultInfo(default_output = out)]

# `pnpm install` for a `package.json` and its `pnpm-lock.yaml`. The output is the package
# directory with its `node_modules`.
pnpm_install = rule(
    impl = _pnpm_install_impl,
    attrs = {
        "srcs": attrs.list(attrs.source()),
    },
)

def _codegen_impl(ctx):
    out = ctx.actions.declare_output(ctx.attrs.out)
    ctx.actions.run(
        cmd_args("python3", ctx.attrs.script, ctx.attrs.src, out.as_output()),
        category = "codegen",
    )
    return [DefaultInfo(default_output = out)]

# Runs `python3 script src out`.
codegen = rule(
    impl = _codegen_impl,
    attrs = {
        "out": attrs.string(),
        "script": attrs.source(),
        "src": attrs.source(),
    },
)

def _vite_app_impl(ctx):
    files = {}
    for src in ctx.attrs.srcs:
        files[src.short_path] = src
    for path, dep in ctx.attrs.generated.items():
        files[path] = dep[DefaultInfo].default_outputs[0]
    app = ctx.actions.symlinked_dir("app", files)
    packages = ctx.attrs.packages[DefaultInfo].default_outputs[0]

    server = cmd_args(
        "python3",
        ctx.attrs.server[DefaultInfo].default_outputs[0],
        "--port",
        str(ctx.attrs.port),
    )
    return [
        DefaultInfo(default_output = app),
        DevInfo(
            server = server,
            outputs = {"app": app, "packages": packages},
            # Vite reads the terminal for its keyboard shortcuts.
            interactive = True,
            watch = [
                # A change to the server script restarts it, and so Vite too.
                DevWatch(inputs = server, restart = True),
                # A new `package.json` or lockfile installs the packages again. The server
                # restarts Vite when they change.
                DevWatch(inputs = packages, build = ["packages"]),
                # Everything else builds the app files again. The server copies the changed
                # files to where Vite serves them, and Vite updates the browser.
                DevWatch(targets = None, build = ["app"]),
            ],
        ),
    ]

# The files of a Vite app: `srcs` at their paths in the package, and the outputs of
# `generated` at the given paths. `buck2 dev` runs Vite on them.
vite_app = rule(
    impl = _vite_app_impl,
    attrs = {
        "generated": attrs.dict(attrs.string(), attrs.dep(), default = {}),
        "packages": attrs.dep(),
        "port": attrs.int(default = 5173),
        "server": attrs.dep(),
        "srcs": attrs.list(attrs.source()),
    },
)
