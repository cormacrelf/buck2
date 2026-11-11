---
id: dev
title: buck2 dev
---

`buck2 dev <target>` starts a long-running process, the _server_, builds
outputs for it when it asks, and tells it when source files change. Use it for a
development server that serves the newest build, a test runner that runs again
on save, or a tool that loads new code into a running program.

A rule makes its targets work with `buck2 dev` by returning the
[`DevInfo`](../../api/build/DevInfo) provider, or only a `RunInfo` (see
[Arguments and `RunInfo` targets](#arguments-and-runinfo-targets)). A bxl
function can return them in place of a target (see
[A bxl function in place of a target](#a-bxl-function-in-place-of-a-target)).
`DevInfo` says:

- `server`: the command that `buck2 dev` starts and keeps running. It starts it
  again when the command changes. See [The server command](#the-server-command).
- `outputs`: the artifacts that `buck2 dev` can build, by name. It builds one
  only when the server asks for it, and tells the server when one that it asked
  for is stale. See [Stale outputs](#stale-outputs).
- `handlers`: bxl functions that build outputs that the server defines while
  it runs. See [Handlers](#handlers).
- `scopes`: named sets of targets, which say where a change was. See
  [Scopes](#scopes).
- `interactive`: whether the server gets the terminal. See
  [The terminal](#the-terminal).
- `local_resources`: processes that the server needs. See
  [Local resources](#local-resources).

When it starts, `buck2 dev` builds the server and starts it. Then it finds what
the server and each output may be built from, which may build what dynamic
actions need (see [What an output is built from](#what-an-output-is-built-from)),
and only then handles the server's requests. It builds no output on its own:
the server asks for the outputs it needs.

## A minimal rule

```python
def _site_impl(ctx):
    www = ctx.actions.copied_dir("www", {p.basename: p for p in ctx.attrs.pages})
    server = cmd_args("python3", ctx.attrs.server)
    return [
        DefaultInfo(default_output = www),
        DevInfo(
            server = server,
            outputs = {"www": www},
        ),
    ]

site = rule(
    impl = _site_impl,
    attrs = {
        "pages": attrs.list(attrs.source()),
        "server": attrs.source(),
    },
)
```

With this rule, `buck2 dev //:site` starts `python3 server.py`. When
`server.py` changes, it starts the server again: the server's command names
that file. The server connects to `buck2 dev` (see
[The server protocol](#the-server-protocol)) and asks for `www`. When a page
changes, `buck2 dev` sends the server `stale`, the server asks for `www` again,
and `buck2 dev` builds it and tells the server where it is.

[`examples/dev_web_server`](https://github.com/facebook/buck2/tree/main/examples/dev_web_server)
has a complete web server that serves the newest build.

## The server command

`buck2 dev` builds the server's command again after a change to a file that the
command may be built from, and after a change to the graph. Usually nothing is
new, and that takes a few milliseconds. If the command line is different, or an
artifact that the command names has different contents, `buck2 dev` stops the
server and starts it again. The artifacts that a command names include the
hidden ones (`cmd_args(hidden = ...)`).

This follows the contents, not the files: saving a file without changing it
does not restart the server, and neither does a change that leaves the
command's artifacts as they were.

So keep the server's command small, and keep what you are working on out of
it. The command should name a script or a small program that stays the same
while you work, and the tools that it runs. The app goes into `outputs`, and
the server starts it:

```python
def _app_impl(ctx):
    exe = ctx.attrs.binary[DefaultInfo].default_outputs[0]
    return [
        DefaultInfo(default_output = exe),
        DevInfo(
            # Only the launcher. A new `exe` does not restart it.
            server = cmd_args("python3", ctx.attrs.launcher),
            outputs = {"exe": exe},
        ),
    ]
```

The launcher connects to `buck2 dev` (see
[The server protocol](#the-server-protocol)), asks for `exe` when it gets
`ready` and when `exe` is stale, and runs the program at the `path` that
`built` gives:

```python
for message in messages:
    if message["type"] in ("ready", "stale"):
        send({"type": "build", "outputs": ["exe"]})
    elif message["type"] == "built":
        exe = message["outputs"]["exe"]
        if exe["changed"] or app is None:
            app = start_again(app, exe["path"])
```

This is better than `server = cmd_args(launcher, exe)`, which restarts the
whole server for every new `exe`:

- The server decides what a new build means: start the app again, load a patch
  into it, or tell a browser to reload.
- The server keeps what it has between builds, such as its connection to
  `buck2 dev` and to the browser.
- A build that fails leaves the app running, and the server gets `failed`.
- `changed` says whether the output is new, so an edit that builds the same
  output does nothing.
- The server decides when to build: at once, after a pause, or when a browser
  asks for a page.

[`examples/subsecond`](https://github.com/facebook/buck2/tree/main/examples/subsecond)
does this: its server starts the app's binary from the `exe` output, and sends
the running app each new `patch` (see [Scopes](#scopes)). Buck may write a new build to the path of the
old one, so that server runs a copy of the binary.

A server that is the app, with nothing around it, works too:
`server = ctx.attrs.binary[RunInfo]`. Each new build of the binary then stops
it and starts it again, and that is all that can happen.

If the server reads a file while it runs that its command does not name, add
the file to the command with `hidden`. A change to a file that the rule cannot
name as an artifact does not restart the server; the server can ask for a
restart itself (see [Requests from the server](#requests-from-the-server)).

## Arguments and `RunInfo` targets

`buck2 dev //:site -- --port 8080` appends `--port 8080` to the server's
command line, as `buck2 run` does. The arguments are the same for the whole
session. They are not part of the command that `buck2 dev` compares to decide on
a restart.

A target without `DevInfo` but with a `RunInfo` works too. Its `RunInfo` is the
server, as in `DevInfo(server = ...)` without outputs. `buck2 dev` starts it,
and starts it again when its command is new (see
[The server command](#the-server-command)). So `buck2 dev //:app` runs a binary,
and runs it again after each change that builds a different binary. A target
with neither provider is an error.

The server inherits the environment of `buck2 dev`, so
`PORT=8080 buck2 dev //:site` gives it `PORT`, and a rule can set a variable
with `cmd_args("env", "PORT=8080", server)`.

## A bxl function in place of a target

`buck2 dev` also takes a [bxl](../../bxl) function, `path/to/file.bxl:name`, in
place of a target. The function returns a list of providers, as the
implementation of a rule does, with a `DefaultInfo`, and a `DevInfo` or a
`RunInfo`. `buck2 dev` uses them as it would use those of a target.

```python
def _dev(ctx):
    target = ctx.configured_targets(ctx.cli_args.target, target_platform = "root//dev:hot_platform")
    exe = ctx.analysis(target).providers()[DefaultInfo].default_outputs[0]
    return [
        DefaultInfo(),
        DevInfo(
            server = cmd_args("python3", ctx.fs.source("tools/launcher.py")),
            outputs = {"exe": exe},
            scopes = {"app": [target.label]},
        ),
    ]

dev = bxl_main(impl = _dev, cli_args = {"target": cli_args.target_label()})
```

The arguments after `--` are the function's, as with `buck2 bxl`. Those after a
second `--` are the server's:

```sh
buck2 dev tools/dev.bxl:dev -- --target //app:main -- --port 8080
```

A function needs no rule and no target of its own, so one function can run many
targets. It can configure the targets as it likes, so a target can build in
one way for `buck2 dev` and in another for `buck2 build`: the target selects on
a constraint, and the function configures it with a platform that has the
constraint.
[`examples/subsecond`](https://github.com/facebook/buck2/tree/main/examples/subsecond)
does that: its targets link for hot patching only on the platform that its
function uses. The outputs can also be artifacts that the function makes with
its own actions (`ctx.bxl_actions().actions`).

What a function's outputs are built from is found as for a target's (see
[What an output is built from](#what-an-output-is-built-from)), but a function
is no target, and owns no source files. Its graph is made of the targets that
the server and the outputs are built from, and their dependencies. An output
that the function's own action makes from an artifact of a target may be built
from any source file of that target. The function's file, and the `.bzl` files
that it loads, count as `.bzl` files of the graph: a change to them makes every
output stale, and `buck2 dev` evaluates the function again.

## Local resources

A server may need other processes, such as a database, an emulator or a
simulator. `DevInfo(local_resources = ...)` names them, as
`ExternalRunnerTestInfo` does for tests: a dictionary from a name to the label
of a target with a [`LocalResourceInfo`](../../api/build/LocalResourceInfo).

```python
DevInfo(
    server = server,
    outputs = {"exe": exe},
    local_resources = {"db": ctx.attrs.db.label},
)
```

The daemon sets them up as it does for tests, and holds them for the session.
Before `buck2 dev` starts the server, the daemon runs the `setup` command of each
resource that does not run, on the local executor. The setup prints JSON and
exits:

```json
{"pid": 4242, "resources": [{"port": "5432"}]}
```

`pid` is the process that holds the resource. The server gets the first entry of
`resources`: an environment variable for each entry of `resource_env_vars`,
with the value of its key. `setup_timeout_seconds` limits the setup, and is 120
seconds if not given.

A resource keeps running while the server restarts. The files that a setup
command names count as files of the server, so a change to them makes the
daemon build the setup command again. A resource is set up again when its setup
command is new, as a server command is new (see
[The server command](#the-server-command)), when its `resource_env_vars` are
new, or when the process of `pid` has exited. The daemon first releases the
old one, so that the new one can use what it had, such as a port. A resource
removed from `local_resources` is released. Whenever a resource was set up
again, `buck2 dev` restarts the server: the server was started against the old
one.

A resource that exits by itself is noticed at the next change that `buck2 dev`
acts on, not at once. `buck2 dev` then says so, the daemon sets it up again,
and the server starts again. Until then the server keeps running.

If a setup fails or times out, `buck2 dev` prints why, with what the setup
printed, and the server does not run: it is not started, or it is
stopped. The next change tries the setup again, and starts the server.

To release a resource, the daemon sends `SIGTERM` to the process of `pid`, and
`SIGKILL` if it has not exited after 20 seconds, as for tests. When `buck2 dev`
exits, it stops the server first, and then waits for the daemon to release the
resources. If `buck2 dev` is killed, the daemon releases them when the session's
connection to it ends. When the daemon is stopped on purpose, by `buck2 kill`,
`buck2 clean`, another version of buck2, or for being inactive, it sends
`SIGTERM` to the resources of every session as it shuts down, but does not wait
for them to exit. Ctrl-Z does not pause the resources.

## Stale outputs

`buck2 dev` builds an output only when the server sends `build` for it. It never
builds one by itself after a change.

From the first time that the server asks for an output, `buck2 dev` _tracks_ it:
it keeps the changes that the output may be built from since the last build of
it that the server got, or since the server asked for it, if it never got one.
After the files stop changing, if a change may affect an output that it tracks,
`buck2 dev` sends `stale`, with each tracked output that has such changes:

```json
{"type": "stale", "outputs": {
  "patch": {"within": ["lib"], "files": ["/project/lib/src/a.rs"]},
  "exe":   {"within": ["lib"], "files": ["/project/lib/src/a.rs"]}}}
```

`files` are the changed files since the server got the output, all of them, not
only the last ones. `within` is described in [Scopes](#scopes). The server
decides what to do, usually to ask for some of the outputs with `build`.

- `stale` comes after every change that may affect a tracked output, also when
  it says what the last one said, such as after a file was saved twice.
- `built` for an output clears its changes, unless the server asked for it with
  `stale_until_ack` (see below). An output that is in `built.failed`, or whose build was
  `failed` or `cancelled` before the server got it, keeps them.
- A build does not send `stale`: the server got `built`, and knows.
- An output that the server never asked for is never in `stale`, and never
  built.
- A server that starts, or starts again, has asked for nothing.

The server's command is not an output: `buck2 dev` builds it again after a
change that it may be built from, and starts the server again if it is new (see
[The server command](#the-server-command)).

A file may belong to the server and to several outputs, and then all of that
happens. A file that belongs to none of them changes nothing.

A build that the daemon stops, because a file that it uses changed, ends in
`cancelled`. The change makes its outputs stale, so `stale` follows once
`buck2 dev` has looked at the change. A server needs one rule: on `stale`,
decide, and send `build`.

### Saying when the server has an output

Getting an output is not always having it: a patch can fail to load, and an app
can take a while to start. A server that wants its outputs to stay stale until
it has them sends `build` with `"stale_until_ack": true`. Their changes then
stay, after `built` too, until the server sends `ack`:

```json
{"type": "ack", "gen": 7, "outputs": ["exe", "patch"]}
```

`ack` says that the server is up to date with these outputs as build `gen` left
them: the changes that `buck2 dev` had heard of when that build started no longer
make them stale, and those since still do. The outputs need not be ones that
the build built. A server that loads patches into a program asks for `exe`, and
once the new program runs, it has every change that `patch` had too, so it sends
`ack` for both. `ack` works whether or not the build asked for
`stale_until_ack`, and has no answer. `buck2 dev` reports an `ack` for a build that has not started, or for a
name that is not an output, and ignores it.

## Scopes

A scope is a named set of targets. It is never built and never asked for: it
only says where a change was.

```python
DevInfo(
    server = server,
    outputs = {"exe": exe, "patch": patch},
    scopes = {"lib": [ctx.attrs.lib.label]},
)
```

A scope is a list of target patterns (`"root//lib/..."`) or labels. They are
matched against the configured dependency graph of the `buck2 dev` target, and
matched again when the graph changes. Scopes may overlap. A change is in a scope
when it is to an own source file of one of the scope's targets, or to one of
those targets themselves. A change to a `.bzl` file is in no scope.

For each output in `stale`, `within` lists the scopes that hold every one of the
output's changes: not the scopes that some change touched, but those that all of
them are in. It is empty when the changes are not all in one scope, when there
are no scopes, after missed changes, and for an output that the server never
got. `ready` names the scopes.

A server that loads patches into a running program uses this. A patch can carry
a change to the patched crate, but not to the code it is linked against:

```python
def on_stale(outputs):
    if "lib" in outputs["exe"]["within"]:
        build(["patch"])           # everything since the binary was built is in lib
    else:
        build(["exe", "patch"])    # something else changed: rebuild, restart the app
```

It reads the entry of `exe`, not of `patch`: `exe`'s changes are those since the
running binary was built, which is what a patch is built against. A patch build
clears `patch`'s changes, and leaves `exe`'s.

### What an output is built from

`buck2 dev` finds the source files of an output target by target, not file by
file:

- Of the target that owns the output, the source files that the target's
  actions build the output from, and no others. So a rule that builds the
  server and the outputs in one target gives each of them only its own source
  files of that target.
- What those actions take from other targets gives the output those targets
  and all their dependencies, with all of their source files, whether or not
  the output is built from each one.

The artifacts that the server's command names are followed in the same way,
and so are the [sidecars](#sidecars) of an output, which count as part of it.

So an output may be stale after a change to a file that it is not built from:
a source file of a dependency that only another output of that dependency uses.
Nothing is new if the server asks for it then, and `built` says so with
`changed`. That keeps `buck2 dev` from walking the action graph below the
output, which takes far longer than walking the targets.

An action that a [dynamic action](dynamic_dependencies.md) of the owning target
creates is only known once the dynamic action has run, so finding what an
output is built from builds what such dynamic actions need, with the files as
they are then, and is done again when what they read changes. If that build
fails, the output gets the source files of the owning target and of its
dependencies instead.

`buck2 dev` does not watch or read files itself: the daemon's file watcher
reports to it the changes in the project that may matter to the target (see
[Limits](#limits)), and once the files stop changing, `buck2 dev` asks the
daemon what each changed file is to the server and to each output. Only files
of the configured dependency graph of the `buck2 dev` target count.

A file that is created and removed again, such as the temporary files some
editors make when they save, changes nothing. A source file that is a symlink
to another file in the project is built from that file, so that file counts
too.

Saving a file without changing it counts as a change: `buck2 dev` does not know
what the files were. The outputs of the file are stale; a build of them builds
nothing new, and the server gets `built` with `changed` false for each output.
The server's command is the same after such a save, so the server keeps
running.

The file watcher must report changes as they happen. The `notify` watcher,
the default outside Meta, does; `buck2 dev` stops with an error if the
`file_watcher` in the `[buck2]` section of `.buckconfig` names one that does
not, such as `fs_hash_crawler`. Files that the project's ignore rules exclude,
and `buck-out`, are never reported.

If the file watcher misses changes, `buck2 dev` cannot tell which files
changed. It then builds the server's command again, and restarts the server if
the command is new. Every tracked output is stale, with the project root in
`files`, and in no scope.

The daemon finds what the server and the outputs are built from once, and again
only when the graph changes: a target, a build file or a `.bzl` file. A change
to a source file is looked up in what it found, so what a change costs does not
grow with the graph. What it finds is which output has which target: it lists
no source files, since the targets already have them.

### Changes to the graph

The daemon also compares the build files of the packages in the graph, the
`.bzl` files that they load, and the targets. When one of them changed,
`buck2 dev` analyses the target again, which builds the server.

- A changed build file, or a file added to or removed from a package, counts
  as a change to the outputs that may be built from the targets it changed,
  in the scopes of those targets.
- A changed `.bzl` file may change what any rule does, so it counts as a
  change to every output, in no scope.
- If the server's command changed, `buck2 dev` starts the server again, as
  after any change (see [The server command](#the-server-command)).

## Sidecars

An output is one artifact. A [`DevOutput`](../../api/build/DevOutput) can have
more with it, by name:

```python
DevInfo(
    server = server,
    outputs = {
        "patch": DevOutput(patch, sidecars = {"symbols": symbols}),
    },
)
```

`buck2 dev` builds the sidecars whenever it builds the output, in the same
build, so they are made from the same files. `built` has each of them under
the output:

```json
{"type": "built", "gen": 7, "outputs": {"patch": {
    "path": "/project/buck-out/.../patch.so", "digest": "...", "changed": true,
    "sidecars": {"symbols": {"path": "/project/buck-out/.../symbols.json", "digest": "...", "changed": false}}}}}
```

Each sidecar says by itself whether it changed since the last build that the
server got, and the `changed` of the output is about the output's own artifact.

A sidecar is not an output: `ready` does not name it, and the server cannot ask
for it. An output whose sidecar fails to build has failed. What a sidecar is
built from counts as what the output may be built from, so a change to it makes
the output stale.

Use a sidecar for a file that says what is in an output, such as a manifest
that the output's action also writes. Or make each file of a directory a
sidecar, named by its path in the directory: the server then knows which files
are new without comparing the directory with the last one.
[`examples/dev_vite`](https://github.com/facebook/buck2/tree/main/examples/dev_vite)
does that.

A sidecar is built by an action, and an action does not know the last build.
So a sidecar can say what an output is, but not how it differs from the last
one, except through `changed`.

## Outputs that run with other files

Some programs run only together with other files: a dynamically linked
executable and its shared libraries, a Python binary and its link tree, a
binary and its resources. Give such an output as an artifact with associated
artifacts, from `artifact.with_associated_artifacts([...])` or
`ctx.actions.write(..., with_inputs = True)`:

```python
launcher = ctx.actions.write(
    "app.sh",
    cmd_args("#!/bin/sh", cmd_args("exec ", exe, ' "$@"', delimiter = "")),
    is_executable = True,
    with_inputs = True,
)
DevInfo(server = server, outputs = {"app": launcher})
```

`buck2 dev` builds the associated artifacts with the output, in the same
build, and materialises them. `path` in `built` is the path of the artifact
itself. The paths that `write` puts into the script are relative to the
project root, which is the server's working directory.

The associated artifacts are part of the output. What they are built from
counts as what the output may be built from. A new associated artifact is a
new output, also when the artifact itself is the same: `changed` is true, and
the `digest` covers the associated artifacts too. A sidecar may have associated
artifacts as well, and they belong to the sidecar in the same way.

### A command line as an output

A rule that wraps a binary of another rule has only its `RunInfo`. The
`RunInfo` says how to run the binary, and names the files that it needs, often
as hidden artifacts. An output may be that `RunInfo`, or anything else that
`cmd_args` takes:

```python
DevInfo(server = server, outputs = {"app": ctx.attrs.binary[RunInfo]})
```

This differs from a target with only a `RunInfo` (see
[Arguments and `RunInfo` targets](#arguments-and-runinfo-targets)). There the
`RunInfo` is the server. Here it is an output, and the server runs it.

`buck2 dev` builds and materialises every artifact that the command line names,
hidden ones too. `built` then has `argv` for the output, and no `path`:

```json
{"type": "built", "gen": 3, "outputs": {"app": {
    "argv": ["/project/buck-out/.../run.sh", "--port", "8080"], "digest": "...", "changed": true}}}
```

`argv` is the command line resolved as the server's own command is: the paths
in it are absolute. The server runs it, and may add arguments. The `digest`
covers the contents of every artifact that the command line names, as for the
server's command. `changed` is true when the digest or `argv` is new.

An output that is a command line works as an artifact does in all else. It may
be a `DevOutput` with `sidecars`. What its artifacts are built from counts as
what the output may be built from. Only an artifact by itself is an artifact
output: `cmd_args(exe)` is a command line, whose `argv` is the path of `exe`.

## Handlers

`DevInfo.outputs` is fixed when the target is analysed. Some servers only know
what they need while they run, and it can be thousands of things. A dev server
for ES modules learns that a page needs `/src/components/Button.tsx` when the
browser asks for it, and only then. Such a server defines outputs itself, with
handlers.

A handler is a bxl function. `DevInfo(handlers = ...)` names them: a dictionary
from a name to the label of a function, `cell//path/to/file.bxl:name`, as
`buck2 bxl` takes it. It works in a `DevInfo` of a rule, and in one that a bxl
function returns. `ready` names the handlers.

```python
DevInfo(
    server = server,
    outputs = {"index": index},
    handlers = {"module": "root//tools/dev.bxl:module"},
)
```

The function returns a list of providers, as a rule does. Its output is the
one artifact in `DefaultInfo.default_outputs`:

```python
def _module(ctx):
    owner = ctx.uquery().owner(ctx.cli_args.path)[0]
    modules = ctx.analysis(ctx.configured_targets(owner.label)).providers()[JsModules]
    return [DefaultInfo(default_output = modules.js[ctx.cli_args.path])]

module = bxl_main(impl = _module, cli_args = {"path": cli_args.string()})
```

The server defines an output in `build`, with a name that it picks, the name of
a handler, and the function's arguments. `outputs` is then an object, from each
name to `null` for an output that is already known, or to a call of a handler:

```json
{"type": "build", "outputs": {
  "index": null,
  "src/main.tsx": {"handler": "module", "args": ["--path", "src/main.tsx"]}}}
```

An output defined like this is a _dynamic output_. From then on it works as an
output of `DevInfo` does: the server can ask for it by name, and it is in
`building`, `built`, `stale` and `ack` under its name, with `streaming` and
`stale_until_ack` as for any output. Defining it again with the same call
changes nothing. Defining it with another call replaces it, and the server has
not got the new one.

- A dynamic output fails, in `built.failed`, if the handler is not in
  `DevInfo.handlers`, if the function fails, or if it returns anything but one
  artifact in `DefaultInfo.default_outputs`. The other outputs of the build go
  on.
- A dynamic output cannot have the name of an output of `DevInfo`. Such a
  definition fails, and the output of `DevInfo` stays as it was.
- A dynamic output with an unknown handler, or with the name of an output of
  `DevInfo`, is not defined, and not tracked.
- A server that starts, or starts again, has defined no output.

`buck2 dev` evaluates each call with the files as they are at each build, and
whenever it says what changes are. The daemon keeps the results, so a call is
evaluated again only after a change to what it used. What a dynamic output may
be built from is found as for the outputs of a bxl function (see
[A bxl function in place of a target](#a-bxl-function-in-place-of-a-target)):
the targets that the walk from the artifact reaches, and their dependencies.
The handler's file, and the `.bzl` files that it loads, count as `.bzl` files
for it: a change to them makes its dynamic outputs stale, in no scope. `within`
uses the scopes of `DevInfo`.

If the function fails, `buck2 dev` does not know what the output is built from.
Every change then makes it stale, until it builds.

`forget` stops tracking outputs, dynamic or not:

```json
{"type": "forget", "outputs": ["src/main.tsx"]}
```

They are not in `stale` again until the server asks for them. A dynamic output
that is forgotten is no longer defined: to ask for it again, the server defines
it again. A server that defines many outputs should forget those that it no
longer needs, such as the modules of a page that is closed.

[`examples/dev_modules`](https://github.com/facebook/buck2/tree/main/examples/dev_modules)
has a dev server that builds each ES module when the browser asks for it.

## The server protocol

`buck2 dev` starts the server in the project root, with the environment
variable `BUCK2_DEV_SOCKET` set to the path of a Unix socket. Python connects
with `socket.AF_UNIX`, and Node with `net.connect(path)`.

The server does not have to connect. A server that never connects is only
started, and started again when its command changes, or after it exits. It
asks for no outputs, so none is built.

A server that connects sends `{"type": "hello", "version": 1}`. `buck2 dev`
answers with `ready`, and from then on writes messages to the connection and
reads requests from it, one JSON object per line. Because the address is in the
environment, a process that the server starts, such as the real program behind
a wrapper script, can connect instead of the server. If a second connection
sends `hello`, it takes over from the first.

`version` is the version of the protocol that the server speaks. If `buck2 dev`
does not speak it, it answers `{"type": "unsupported", "versions": [1]}`, with
the versions that it does speak, and nothing else until the server sends
`hello` again, on the same connection, with one of them. `hello` and
`unsupported` keep this shape in every version.

Within a version, `buck2 dev` may add new message types and new fields. A
server must ignore messages whose `type` it does not know, and fields it does
not know.
`buck2 dev` reports a line that is not a valid request, and ignores it. It
closes the connection of a server that sends a line longer than 4 MiB, or that
stops reading, so that a thousand messages wait for it. The server keeps
running.

The server's stdout and stderr go to the terminal; see
[The terminal](#the-terminal).

### Stopping the server

The server runs in a process group of its own, so that stopping it stops the
processes it started too. To stop the
server, before a restart and when `buck2 dev` exits:

1. If the server is connected, `buck2 dev` sends it `shutdown`, and waits up to
   5 seconds for it to exit.
2. If the server, or any process in its group, is still running, `buck2 dev`
   sends the group `SIGTERM`, and waits up to 5 seconds.
3. It kills what is left.

`buck2 dev` waits for this before it starts the server again, so that the new
server can use the old one's resources, such as a port, and before it exits.
If files change while it waits, it builds the new server again from them before
it starts it, and those changes do not restart the new server.
Ctrl-C while it waits kills the server at once.

When the server exits, or fails to start again, `buck2 dev` prints that, and
keeps watching. The next change to what the server or an output is built from
starts the server again, built from the files as they are then. So a server can
also run to completion on each change, such as a test runner. `buck2 dev` ends on Ctrl-C,
`SIGTERM` or `SIGHUP`, or when an interactive server (see
[The terminal](#the-terminal)) exits successfully.

### The terminal

By default the server stays in the background. Its stdin is empty, and Ctrl-C
reaches only `buck2 dev`, which stops the server and exits.

When `buck2 dev` uses the superconsole, it keeps it for the whole session. Each
build shows its progress there, and between builds one line says how many
targets `buck2 dev` watches, whether the server runs, and how the last build
went. The
server's stdout and stderr are one pseudo-terminal that `buck2 dev` reads, so
the server still writes to a terminal and keeps its colours and line buffering.
Its lines appear above the superconsole. Other terminal control, such as moving
the cursor, is dropped; a server that needs it should be interactive. With the
simple console, or when stderr is not a terminal, the server writes to stdout
and stderr itself.

`buck2 dev` does not print the build ID or the cache statistics of each build.
It prints build errors in full.

With `DevInfo(interactive = True)`, the server inherits stdin. When stdin is a
terminal, the server's process group becomes the terminal's foreground group,
so it gets the keys typed and Ctrl-C. Use this for a server with keyboard
shortcuts, a REPL, or a debugger. If Ctrl-C kills the server, `buck2 dev`
exits. `buck2 dev` takes the terminal back, and restores its settings, whenever
the server stops.

Ctrl-Z stops the server with `buck2 dev`, and returns the terminal to the
shell. `fg` continues both, and gives an interactive server the terminal back
with its settings. After `bg`, an interactive server that reads the terminal
stops until `fg`. A server that is not interactive but opens the terminal
itself, such as `/dev/tty`, would wait for it forever; `buck2 dev` says so and
stops it.

An interactive server has the terminal to itself: `buck2 dev` uses the simple
console, and prints only errors and that the server exited or restarts.

### Messages to the server

| `type`          | Fields                                                    | Sent when                                                                                                    |
| --------------- | --------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| `ready`         | `version`: as in `hello`, `outputs`: the names in `DevInfo.outputs`, `scopes`: the names in `DevInfo.scopes`, `handlers`: the names in `DevInfo.handlers` | The server sent `hello` with a version that `buck2 dev` speaks.          |
| `unsupported`   | `versions`: the versions that `buck2 dev` speaks          | The server sent `hello` with another version.                                                                |
| `building`      | `gen`, `outputs`: names, `files`: the changes of these outputs since the server got them | The server asked for a build, and it starts.                                     |
| `built`         | `gen`, `outputs`: name to `{path, digest, changed}`, with `sidecars`: name to `{path, digest, changed}` for an output that has [sidecars](#sidecars), `failed`: name to error if some outputs failed, and `last_of_generation` | A build of at least one output succeeded, or, for a streaming build, an output finished. `path` is absolute. An output that is a [command line](#a-command-line-as-an-output) has `argv` in place of `path`. `changed` is false when the output is the same as at the last build. The outputs in `failed` did not build. `last_of_generation` is true on the last `built` of the build. |
| `failed`        | `gen`, `error`                                            | A build of every output, unless it streams, or the analysis of the target, failed. For an action that failed, `error` has what the terminal shows for it: the command, its exit code, and the end of its output. |
| `cancelled`     | `gen`                                                     | A source file changed during the build, so it was stopped. Its outputs that the server did not get are in the next `stale`. |
| `stale`         | `outputs`: name to `{within, files}`, for each output that the server asked for and that changes since it got it may affect. `within`: the scopes that hold all of those changes. `files`: the changed files, absolute | Files changed that may affect an output that the server asked for. After missed changes the project root is in `files`. See [Stale outputs](#stale-outputs) and [Scopes](#scopes). |
| `graph_changed` |                                                           | A build file, a `.bzl` file or a target changed, or an analysis that failed works again, and `buck2 dev` analysed the target again. |
| `shutdown`      |                                                           | `buck2 dev` is stopping the server. See [Stopping the server](#stopping-the-server).                         |

`gen` is a number that increases with each build. `building` and `built` for
the same build have the same `gen`. A `building` is followed by a `built` whose
`last_of_generation` is true, or by `failed` or `cancelled`, unless `buck2 dev`
exits first.

By default a build sends one `built`, once every output is done. A server that
sends `build` with `"streaming": true` gets a `built` for each output as soon
as it is done, with `last_of_generation` false on all but the last, so that a
fast output does not wait for a slow one. An output that fails comes in a
`built` of its own, in `failed`, and the build never ends in `failed` for it.
If the build is `cancelled`, the server keeps the outputs that it got, and only
the others are stale. Once an output is done, a change to a file that only it is
built from no longer stops the build. A server gets `built`, `failed` or `cancelled` for a
build only if it got that build's `building`: a server that connects while a
build runs hears nothing of that build, and asks for what it needs after
`ready`. A `failed` without a `building` is about the analysis of the target,
or the build of the server's command.
Messages
are sent only to a server that has sent `hello`. If a second connection sends
`hello` while outputs are stale, its `ready` is followed by `stale`.

While the daemon builds, it watches the files that the build depends on: the
files that the outputs it builds may be built from, and the build files and
`.bzl` files of the graph. If one of them gets different contents, the daemon
stops the build, and `buck2 dev` sends `cancelled`, asks about the changes once
the files stop changing, and sends `stale` with the stopped build's outputs.
Saving a file without changing it, and a change to a file that the build does
not use, do not stop a build. Other
changes during a build, and changes while the daemon analyses the target or says
what changed, are asked about once that is done. If analysing fails,
`buck2 dev` sends `failed`, and analyses again after the next change, and
before the next build that the server asks for, which then gets `building` and
`failed` if the analysis still fails. The same errors again, after a change that did not touch what failed,
are not sent again. Once the analysis works again, the server gets
`graph_changed`.

### Requests from the server

| `type`    | Fields                    | Effect                                                                                          |
| --------- | ------------------------- | ----------------------------------------------------------------------------------------------- |
| `hello`   | `version`: 1              | Start talking to `buck2 dev`. It answers with `ready`, or `unsupported`. Send it before anything else. |
| `build`   | `outputs`: a list of names, or an object from names to `null` or to `{handler, args}`, and optionally `streaming` and `stale_until_ack`, both false by default | Build these outputs now, with all changes, and track them from now on. A name with `{handler, args}` defines a dynamic output (see [Handlers](#handlers)). If the last analysis failed, `buck2 dev` analyses the target first, and if the server's command is new, restarts the server instead. With `streaming`, each output comes in its own `built` as soon as it is done. With `stale_until_ack`, the outputs stay stale until the server sends `ack`. |
| `ack`     | `gen`, `outputs`: a list of names | The server is up to date with these outputs as build `gen` left them. See [Saying when the server has an output](#saying-when-the-server-has-an-output). |
| `forget`  | `outputs`: a list of names | Stop tracking these outputs. A dynamic output among them is no longer defined. No answer. |
| `restart` |                           | Build the server again and restart it. The new server has asked for nothing. If the build fails, the old server keeps running until the next analysis restarts it. |

`buck2 dev` reads the connection all the time, also while it builds, so a
server never waits for it to read. Requests that arrive during a build are
handled after it.

A typical server sends `hello`, then `{"type": "build", "outputs": [...]}` when
it gets `ready`, and again for the outputs that it wants when it gets `stale`.
It uses each `built` message. A server for a site needs no more than this:

```python
for message in messages:
    if message["type"] == "ready":
        send({"type": "build", "outputs": ["www"]})
    elif message["type"] == "stale":
        send({"type": "build", "outputs": list(message["outputs"])})
    elif message["type"] == "built":
        serve(message["outputs"]["www"]["path"])
```

## Other buck2 commands

Each build and each analysis that `buck2 dev` does is a separate, short daemon
command. Between them `buck2 dev` holds no lock in the daemon, only the
server's [local resources](#local-resources), so other commands, such as
`buck2 build` in another terminal, run as usual.

If another command starts while a `buck2 dev` command runs, and it sees
different files or a different configuration, it does not wait for that
command. It stops the `buck2 dev` command, and `buck2 dev` sends it again when
the other command lets it in.

If the daemon goes away, `buck2 dev` stops its server and exits with an error.
It does not start the daemon again. A daemon is usually stopped on purpose, by
`buck2 kill`, by `buck2 clean`, or by running a different version of buck2 in
the project, and starting it again to carry on building would undo that.

## Limits

- `buck2 dev` takes one target, or one bxl function.
- A bxl function is evaluated again only after a change to what its graph is
  made of (see [A bxl function in place of a target](#a-bxl-function-in-place-of-a-target)).
  A change to a target that the function looks at, but that neither the server
  nor an output is built from, is not seen until another change is.
- A change to a file in a package of the graph costs a short daemon command,
  even if nothing is built from the file: it says what the file is. A server
  that keeps writing files in such a package keeps `buck2 dev` asking. Changes
  elsewhere cost nothing: the daemon does not report them to `buck2 dev`,
  except to build files and `.bzl` files of the graph, `PACKAGE` files,
  `.buckconfig` files, and the directories above the packages.
- When a target, a build file or a `.bzl` file of the graph changes, the daemon
  walks the graph of the target again to find what the server and the outputs
  are built from.
- Each dynamic output has a graph of its own, and each change is looked up in
  each of them. So what a change costs grows with the number of dynamic
  outputs: a few hundred are fine.
- A file that a `.buckconfig` includes under another name is not reported, so
  a change to it is only seen with the next change that is.
- A `.buckconfig` change is not a change to what the server or an output is
  built from, but `buck2 dev` asks the daemon about it as about any other file,
  and the daemon reads the configuration again. If that changes the targets of the graph, such as a
  source file that `read_config` names, it is a change to them, as in
  [Changes to the graph](#changes-to-the-graph). Other configuration changes
  are used by the next build.
- `buck2 dev` runs only on Unix so far.
