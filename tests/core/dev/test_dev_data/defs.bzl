# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

ConcatInfo = provider(fields = ["out", "outs"])

# The outputs of a `concat` and of its dependencies.
ConcatOuts = transitive_set(args_projections = {"out": lambda out: out})

def _concat_output(ctx, name):
    """Concatenate the outputs of `deps` and then `srcs`, after sleeping for the number of
    seconds in `delay` if it is set."""
    out = ctx.actions.declare_output(name, has_content_based_path = False)
    inputs = [d[ConcatInfo].out for d in ctx.attrs.deps] + ctx.attrs.srcs
    delay = ctx.attrs.delay if ctx.attrs.delay else "/dev/null"
    ctx.actions.run(
        cmd_args(
            "sh",
            "-c",
            'out="$1"; delay="$2"; shift 2; sleep "$(cat "$delay")" 2>/dev/null; cat "$@" > "$out"',
            "--",
            out.as_output(),
            delay,
            inputs,
        ),
        category = "concat",
        local_only = True,
    )
    return out

def _concat(ctx):
    out = _concat_output(ctx, "out.txt")
    outs = ctx.actions.tset(
        ConcatOuts,
        value = out,
        children = [d[ConcatInfo].outs for d in ctx.attrs.deps],
    )
    return [DefaultInfo(default_output = out), ConcatInfo(out = out, outs = outs)]

concat = rule(
    impl = _concat,
    attrs = {
        "delay": attrs.option(attrs.source(), default = None),
        "deps": attrs.list(attrs.dep(), default = []),
        "srcs": attrs.list(attrs.source(), default = []),
    },
)

def _dev_app(ctx):
    site = _concat_output(ctx, "site.txt")
    server_script = ctx.attrs.server
    if ctx.attrs.server_check:
        # Build the server by copying it, which fails while `server_check` says `broken`.
        server_script = ctx.actions.declare_output("server.py")
        ctx.actions.run(
            cmd_args(
                "sh",
                "-c",
                'if grep -q broken "$1"; then exit 1; fi; cp "$2" "$3"',
                "--",
                ctx.attrs.server_check,
                ctx.attrs.server,
                server_script.as_output(),
            ),
            category = "server",
            local_only = True,
        )
    server = cmd_args("python3", server_script, ctx.attrs.server_args)
    if ctx.attrs.mode == "auto":
        # The app's own files build the site; any other change waits for the server.
        watch = [
            DevWatch(targets = [ctx.label], build = ["site"]),
            DevWatch(targets = None, notify = True),
        ]
    elif ctx.attrs.mode == "all":
        watch = [DevWatch(targets = None, build = ["site"])]
    elif ctx.attrs.mode == "restart":
        watch = [DevWatch(targets = None, restart = True)]
    elif ctx.attrs.mode == "inputs":
        # The server script is a source file of this target, like `srcs`.
        watch = [
            DevWatch(inputs = server, restart = True),
            DevWatch(targets = None, build = ["site"]),
        ]
    elif ctx.attrs.mode == "deep":
        # `deep` is built by an action that a dynamic action makes, from a transitive set of
        # the outputs of `deps`. Its `inputs` set reaches their source files only through both.
        outs = ctx.actions.tset(
            ConcatOuts,
            children = [d[ConcatInfo].outs for d in ctx.attrs.deps],
        )
        trigger = ctx.actions.write("trigger.txt", "")
        deep = ctx.actions.declare_output("deep.txt")

        def _deep(ctx, artifacts, outputs):
            ctx.actions.run(
                cmd_args(
                    "sh",
                    "-c",
                    'out="$1"; shift; cat "$@" > "$out"',
                    "--",
                    outputs[deep].as_output(),
                    outs.project_as_args("out"),
                ),
                category = "deep",
                local_only = True,
            )

        ctx.actions.dynamic_output(
            dynamic = [trigger],
            inputs = [],
            outputs = [deep.as_output()],
            f = _deep,
        )
        watch = [
            DevWatch(inputs = deep, restart = True),
            DevWatch(targets = None, build = ["site"]),
        ]
    elif ctx.attrs.mode == "split":
        watch = [
            DevWatch(targets = [ctx.label], build = ["site"]),
            DevWatch(targets = None, restart = True),
        ]
    else:
        fail("unknown mode " + ctx.attrs.mode)
    return [
        DefaultInfo(default_output = site),
        DevInfo(
            server = server,
            outputs = {"site": site},
            watch = watch,
        ),
    ]

dev_app = rule(
    impl = _dev_app,
    attrs = {
        "delay": attrs.option(attrs.source(), default = None),
        "deps": attrs.list(attrs.dep(), default = []),
        "mode": attrs.string(),
        "server": attrs.source(),
        "server_args": attrs.list(attrs.string(), default = []),
        "server_check": attrs.option(attrs.source(), default = None),
        "srcs": attrs.list(attrs.source(), default = []),
    },
)
