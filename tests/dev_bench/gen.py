# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

"""Generate a project to time `buck2 dev` on.

usage: gen.py <dir> <packages> <targets per package> <sources per target>

Each target is a `lib` with its own source files and two dependencies on targets of earlier
packages. `//app:<mode>` depends on the last targets of every package, so its graph is all of
them. The modes differ in their outputs:

- `outputs`: a trivial output, and one that is built from every library.
- `outputs8`: the same, with eight outputs that are built from every library, to see what
  each further output costs.
"""

import os
import sys

root, packages, targets, sources = sys.argv[1], *map(int, sys.argv[2:5])


def write(rel: str, content: str) -> None:
    path = os.path.join(root, rel)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(content)


write(
    ".buckconfig",
    """[cells]
  root = .
  nano_prelude = nano_prelude

[cell_aliases]
  prelude = nano_prelude

[external_cells]
  nano_prelude = bundled

[buck2]
  file_watcher = notify
""",
)
write(".buckroot", "")
write(
    "defs.bzl",
    '''LibInfo = provider(fields = ["out", "outs"])

LibOuts = transitive_set(args_projections = {"out": lambda out: out})

def _lib(ctx):
    out = ctx.actions.declare_output("out.txt")
    ctx.actions.run(
        cmd_args("sh", "-c", 'out="$1"; shift; cat "$@" > "$out"', "--", out.as_output(), ctx.attrs.srcs),
        category = "lib",
        local_only = True,
    )
    outs = ctx.actions.tset(LibOuts, value = out, children = [d[LibInfo].outs for d in ctx.attrs.deps])
    return [DefaultInfo(default_output = out), LibInfo(out = out, outs = outs)]

lib = rule(
    impl = _lib,
    attrs = {
        "deps": attrs.list(attrs.dep(), default = []),
        "srcs": attrs.list(attrs.source(), default = []),
    },
)

def _app(ctx):
    outs = ctx.actions.tset(LibOuts, children = [d[LibInfo].outs for d in ctx.attrs.deps])
    small = ctx.actions.write("small.txt", ctx.attrs.small)
    server = cmd_args("python3", ctx.attrs.server)
    # The outputs built from every library. Their inputs are in a file: there are too many
    # paths for a command line.
    outputs = {"small": small}
    for i in range(int(ctx.attrs.mode.removeprefix("outputs") or 1)):
        name = "everything{}".format(i)
        paths = ctx.actions.write(name + ".args", outs.project_as_args("out"), with_inputs = True)
        more = ctx.actions.declare_output(name + ".txt")
        ctx.actions.run(
            cmd_args("sh", "-c", 'xargs cat < "$1" > "$2"', "--", paths, more.as_output()),
            category = "everything",
            identifier = name,
            local_only = True,
        )
        outputs[name] = more
    return [
        DefaultInfo(default_output = small),
        DevInfo(server = server, outputs = outputs),
    ]

app = rule(
    impl = _app,
    attrs = {
        "deps": attrs.list(attrs.dep(), default = []),
        "mode": attrs.string(),
        "server": attrs.source(),
        "small": attrs.string(default = "small"),
    },
)
''',
)
write(
    "app/server.py",
    '''import json, os, socket, sys, time

log = open(os.path.join("buck-out", "bench_events.jsonl"), "a")

def say(message):
    message["at"] = time.time()
    log.write(json.dumps(message) + "\\n")
    log.flush()

say({"type": "started"})
conn = socket.socket(socket.AF_UNIX)
conn.connect(os.environ["BUCK2_DEV_SOCKET"])
stream = conn.makefile("rwb")
stream.write(b'{"type": "hello", "version": 1}\\n')
stream.flush()

def build(outputs):
    stream.write((json.dumps({"type": "build", "outputs": outputs}) + "\\n").encode())
    stream.flush()

for line in stream:
    message = json.loads(line)
    outputs = message.pop("outputs", None)
    say(message)
    # Every output when it starts, and every output that is stale after a change.
    if message["type"] in ("ready", "stale"):
        build(list(outputs))
    elif message["type"] == "shutdown":
        break
''',
)

tops = []
for p in range(packages):
    lines = ['load("//:defs.bzl", "lib")', ""]
    for t in range(targets):
        srcs = [f"t{t}/s{s}.txt" for s in range(sources)]
        for src in srcs:
            write(f"pkg/p{p}/{src}", f"p{p} t{t}\n")
        deps = []
        # Chains of 50 packages, so that the graph is wide and not too deep.
        if p % 50 != 0:
            first = p - p % 50
            deps = [
                f"//pkg/p{p - 1}:t{t}",
                f"//pkg/p{first + (p * 7 + t) % (p - first)}:t{(t + 1) % targets}",
            ]
        lines.append(
            f'lib(name = "t{t}", srcs = {srcs!r}, deps = {sorted(set(deps))!r}, visibility = ["PUBLIC"])'.replace("'", '"')
        )
    write(f"pkg/p{p}/BUCK", "\n".join(lines) + "\n")
tops = [
    f"//pkg/p{p}:t{t}"
    for p in range(packages)
    if (p + 1) % 50 == 0 or p == packages - 1
    for t in range(targets)
]

app = ['load("//:defs.bzl", "app")', ""]
for mode in ("outputs", "outputs8"):
    app.append(
        f'app(name = "{mode}", mode = "{mode}", server = "server.py", deps = {tops!r})'.replace(
            "'", '"'
        )
    )
write("app/BUCK", "\n".join(app) + "\n")
print(f"{packages * targets} targets, {packages * targets * sources} source files")
