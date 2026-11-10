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

def _concat_output(ctx, name, inputs = None):
    """Concatenate the outputs of `deps` and then `srcs`, or `inputs` if given, after sleeping
    for the number of seconds in `delay` if it is set. The action fails if a line of the result
    starts with `FAIL`."""
    out = ctx.actions.declare_output(name, has_content_based_path = False)
    if inputs == None:
        inputs = [d[ConcatInfo].out for d in ctx.attrs.deps] + ctx.attrs.srcs
    delay = ctx.attrs.delay if ctx.attrs.delay else "/dev/null"
    ctx.actions.run(
        cmd_args(
            "sh",
            "-c",
            'out="$1"; delay="$2"; shift 2; sleep "$(cat "$delay")" 2>/dev/null; cat "$@" > "$out"; '
            + 'if grep -q "^FAIL" "$out"; then echo "concat: an input says FAIL" >&2; exit 1; fi',
            "--",
            out.as_output(),
            delay,
            inputs,
        ),
        category = "concat",
        identifier = name,
        local_only = True,
    )
    return out

def _concat(ctx):
    if ctx.attrs.fail_analysis:
        fail("concat: `fail_analysis` is set")
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
        "fail_analysis": attrs.bool(default = False),
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
    server = cmd_args(ctx.attrs.server_exe, server_script, ctx.attrs.server_args)
    site_name = ctx.attrs.site_name

    # More outputs, each the concatenation of its own source files.
    extra = {name: _concat_output(ctx, name + ".txt", inputs = srcs) for name, srcs in ctx.attrs.extra_outputs.items()}
    if ctx.attrs.mode == "auto":
        # The app's own files build the site; any other change waits for the server.
        watch = [
            DevWatch(targets = [ctx.label], build = [site_name]),
            DevWatch(targets = None, notify = True),
        ]
    elif ctx.attrs.mode == "all":
        watch = [DevWatch(targets = None, build = [site_name])]
    elif ctx.attrs.mode == "restart":
        watch = [DevWatch(targets = None, restart = True)]
    elif ctx.attrs.mode == "inputs":
        # The server script is a source file of this target, like `srcs`.
        watch = [
            DevWatch(inputs = server, restart = True),
            DevWatch(targets = None, build = [site_name]),
        ]
    elif ctx.attrs.mode == "deep":
        # `deep` is built by an action that a dynamic action makes, from a transitive set of
        # the outputs of `deps`. Its `inputs` set reaches their source files only through both.
        outs = ctx.actions.tset(
            ConcatOuts,
            children = [d[ConcatInfo].outs for d in ctx.attrs.deps],
        )
        if ctx.attrs.watch_delay:
            # Finding the files of the `inputs` set builds this, which takes the number of
            # seconds in `watch_delay`.
            trigger = ctx.actions.declare_output("trigger.txt")
            ctx.actions.run(
                cmd_args(
                    "sh",
                    "-c",
                    'sleep "$(cat "$1")"; : > "$2"',
                    "--",
                    ctx.attrs.watch_delay,
                    trigger.as_output(),
                ),
                category = "trigger",
                local_only = True,
            )
        else:
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
            DevWatch(targets = None, build = [site_name]),
        ]
    elif ctx.attrs.mode == "multi":
        # Each extra output is built when its own source files change, `site` for the rest.
        watch = [DevWatch(inputs = extra[name], build = [name]) for name in sorted(extra)]
        watch.append(DevWatch(targets = None, build = [site_name]))
    elif ctx.attrs.mode == "patch":
        # As a rule that patches a running program has it: `patch` is built from `srcs` only,
        # the files that it is built from build it, and any other change waits.
        extra["patch"] = _concat_output(ctx, "patch.txt", inputs = ctx.attrs.srcs)
        watch = [
            DevWatch(inputs = extra["patch"], build = ["patch"]),
            DevWatch(targets = None, notify = True),
        ]
    elif ctx.attrs.mode == "unwatched":
        # No set claims any file.
        watch = []
    elif ctx.attrs.mode == "wait":
        # Every change waits, also one to what the server is built from.
        watch = [DevWatch(targets = None, notify = True)]
    elif ctx.attrs.mode == "kept":
        # A set that does nothing keeps the server's files from the `notify` set.
        watch = [
            DevWatch(inputs = server),
            DevWatch(targets = None, notify = True),
        ]
    elif ctx.attrs.mode == "split":
        watch = [
            DevWatch(targets = [ctx.label], build = [site_name]),
            DevWatch(targets = None, restart = True),
        ]
    elif ctx.attrs.mode == "outputs":
        # No watch list: `buck2 dev` finds what the server and each output may be built from.
        watch = None
    elif ctx.attrs.mode == "only":
        # As a rule that patches a running program has it, without a watch list. `exe` and
        # `patch` are built from `srcs` and `deps`, like `site`. `buck2 dev` builds `patch`
        # again by itself only after a change to this target's own files, and never `exe`.
        extra["exe"] = _concat_output(ctx, "exe.txt")
        extra["patch"] = _concat_output(ctx, "patch.txt")
        watch = None
    elif ctx.attrs.mode == "flood":
        # No watch list. The action of `flood` writes many files under `buck-out` at once,
        # as the actions of a large build do together, before it concatenates `srcs`.
        flood = ctx.actions.declare_output("flood.txt")
        ctx.actions.run(
            cmd_args(
                "sh",
                "-c",
                'out="$1"; shift; dir="$BUCK_SCRATCH_PATH/flood"; mkdir -p "$dir"; sleep 1; '
                + "for p in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16; do "
                + '( i=0; while [ $i -lt 6000 ]; do : > "$dir/$p.$i"; i=$((i+1)); done ) & '
                + 'done; wait; cat "$@" > "$out"',
                "--",
                flood.as_output(),
                ctx.attrs.srcs,
            ),
            category = "flood",
            local_only = True,
        )
        extra["flood"] = flood
        watch = None
    elif ctx.attrs.mode == "sidecars":
        # No watch list, and one output. The extra outputs are its sidecars, and so is its
        # first source file.
        watch = None
    else:
        fail("unknown mode " + ctx.attrs.mode)
    outputs = {site_name: site}
    outputs.update(extra)
    if ctx.attrs.mode == "only":
        outputs["exe"] = DevOutput(outputs["exe"], only = [])
        outputs["patch"] = DevOutput(outputs["patch"], only = [ctx.label])
    if ctx.attrs.mode == "sidecars":
        sidecars = dict(extra)
        sidecars["src"] = ctx.attrs.srcs[0]
        outputs = {site_name: DevOutput(site, sidecars = sidecars)}
    if not ctx.attrs.dev_info:
        return [DefaultInfo(default_output = site)]
    return [
        DefaultInfo(default_output = site),
        DevInfo(
            server = server,
            outputs = outputs,
            watch = watch,
        ),
    ]

dev_app = rule(
    impl = _dev_app,
    attrs = {
        "delay": attrs.option(attrs.source(), default = None),
        "deps": attrs.list(attrs.dep(), default = []),
        # Whether to return `DevInfo`.
        "dev_info": attrs.bool(default = True),
        "extra_outputs": attrs.dict(attrs.string(), attrs.list(attrs.source()), default = {}),
        "mode": attrs.string(),
        "server": attrs.source(),
        "server_args": attrs.list(attrs.string(), default = []),
        "server_check": attrs.option(attrs.source(), default = None),
        # The program that runs `server`.
        "server_exe": attrs.string(default = "python3"),
        # The name of the output that `srcs` and `deps` build.
        "site_name": attrs.string(default = "site"),
        "srcs": attrs.list(attrs.source(), default = []),
        "watch_delay": attrs.option(attrs.source(), default = None),
    },
)
