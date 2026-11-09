# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

ConcatInfo = provider(fields = ["out"])

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
    return [DefaultInfo(default_output = out), ConcatInfo(out = out)]

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
    else:
        fail("unknown mode " + ctx.attrs.mode)
    return [
        DefaultInfo(default_output = site),
        DevInfo(
            server = cmd_args("python3", ctx.attrs.server, ctx.attrs.server_args),
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
        "srcs": attrs.list(attrs.source(), default = []),
    },
)
