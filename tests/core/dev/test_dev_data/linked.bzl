# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# Rules for `test_dev_linked`: binaries that run only together with other files.

def _copy(ctx, name, src):
    """A copy of `src`, built by an action, so that it exists only once it is materialised."""
    out = ctx.actions.declare_output(name, has_content_based_path = False)
    ctx.actions.run(
        cmd_args("cp", src, out.as_output()),
        category = "copy",
        identifier = name,
        local_only = True,
    )
    return out

def _check_script(name):
    """The lines of a script that prints the content of the file at `$name`, or that it is
    missing, and then its arguments."""
    return [
        'if [ -f "${name}" ]; then echo "{name}: $(cat "${name}")"; else echo "missing: ${name}"; fi'.format(name = name),
        'echo "args: $*"',
    ]

def _script(ctx, name, var, file):
    """A script that checks for `file`, which it names by its path from the project root, where
    the server runs it. It does not read `file` when it is built, and nothing builds `file` for
    it."""
    return ctx.actions.write(
        name,
        cmd_args(
            "#!/bin/sh",
            cmd_args(var, "=", file, delimiter = ""),
            _check_script(var),
        ),
        is_executable = True,
    )

def _associated_app(ctx):
    lib = _copy(ctx, "lib.txt", ctx.attrs.lib)
    exe = _script(ctx, "app.sh", "lib", lib)
    app = exe.with_associated_artifacts([lib])
    if ctx.attrs.extra:
        extra = _copy(ctx, "extra.txt", ctx.attrs.extra)
        check = _script(ctx, "check.sh", "extra", extra)
        app = DevOutput(app, sidecars = {"check": check.with_associated_artifacts([extra])})
    return [
        DefaultInfo(default_output = exe),
        DevInfo(
            server = cmd_args("python3", ctx.attrs.server),
            outputs = {"app": app},
        ),
    ]

# A `DevInfo` with one output, `app`: a script, with an associated artifact that it reads when
# it runs, a copy of `lib`. With `extra`, `app` has a sidecar `check`: a script, with an
# associated artifact, a copy of `extra`.
associated_app = rule(
    impl = _associated_app,
    attrs = {
        "extra": attrs.option(attrs.source(), default = None),
        "lib": attrs.source(),
        "server": attrs.source(),
    },
)
