---
id: dev
title: buck2 dev
---

`buck2 dev <target>` starts a long-running process, the _server_, and builds
outputs for it again when source files change. Use it for a development server
that serves the newest build, a test runner that runs again on save, or a tool
that loads new code into a running program.

A rule makes its targets work with `buck2 dev` by returning the
[`DevInfo`](../../api/build/DevInfo) provider. `DevInfo` says three things:

- `server`: the command that `buck2 dev` starts and keeps running. It starts it
  again when the command changes. See [The server command](#the-server-command).
- `outputs`: the artifacts that `buck2 dev` can build, by name.
- `watch`: a list of [`DevWatch`](../../api/build/DevWatch) values. They say
  what to do when a source file changes. Without `watch`, `buck2 dev` works
  that out itself; see [Without a watch list](#without-a-watch-list).
- `interactive`: whether the server gets the terminal. See
  [The terminal](#the-terminal).

When it starts, `buck2 dev` builds the server and starts it. Then it finds the
files to watch, which builds what a `DevWatch` with `inputs` needs (see
[Watch sets](#watch-sets)), and only then handles the server's requests. It
builds nothing else on its own: the server asks for the outputs it needs.

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
            watch = [
                DevWatch(targets = None, build = ["www"]),
            ],
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
that file. When any other source file in the dependency graph of `//:site`
changes, it builds `www` again and tells the server where it is, if the server
has connected to `buck2 dev` (see [The server protocol](#the-server-protocol)).

[`examples/dev_web_server`](https://github.com/facebook/buck2/tree/main/examples/dev_web_server)
has a complete web server that serves the newest build.

## The server command

`buck2 dev` builds the server's command again after every change that it acts
on, whichever watch set claims the changed file, and also when none does.
Usually nothing is new, and that takes a few milliseconds. If the command line
is different, or an artifact that the command names has different contents,
`buck2 dev` stops the server and starts it again. The artifacts that a command
names include the hidden ones (`cmd_args(hidden = ...)`).

No watch set is needed for this, and it follows the contents, not the files:
saving a file without changing it does not restart the server, and neither
does a change that leaves the command's artifacts as they were.

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
            watch = [DevWatch(targets = None, build = ["exe"])],
        ),
    ]
```

The launcher connects to `buck2 dev` (see
[The server protocol](#the-server-protocol)), asks for `exe` when it gets
`ready`, and runs the program at the `path` that `built` gives:

```python
for message in messages:
    if message["type"] == "ready":
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

[`examples/subsecond`](https://github.com/facebook/buck2/tree/main/examples/subsecond)
does this: its server starts the app's binary from the `exe` output, and sends
the running app each new `patch`. Buck may write a new build to the path of the
old one, so that server runs a copy of the binary.

A server that is the app, with nothing around it, works too:
`server = ctx.attrs.binary[RunInfo]`. Each new build of the binary then stops
it and starts it again, and that is all that can happen.

If the server reads a file while it runs that its command does not name, add
the file to the command with `hidden`. For files that the rule cannot name as
artifacts, such as the source files of every target in the graph, use a watch
set with `restart` (see [Watch sets](#watch-sets)). Such a set restarts the
server at every save of its files, changed or not.

## Without a watch list

This is a prototype.

A `DevInfo` without `watch` has no rules. `buck2 dev` finds the source files
that the server's command and each output may be built from, and after a
change:

- to a file of the server's command, it builds the command again, and starts
  the server again if the command is new (see
  [The server command](#the-server-command));
- to a file of an output, it builds that output again, and sends `built`.

```python
DevInfo(server = server, outputs = {"www": www})
```

A file may belong to the server and to several outputs, and then all of that
happens. A file that belongs to none of them changes nothing, and costs no
build. What an output may be built from is found as for `inputs` in
[What `inputs` claim](#what-inputs-claim): of the target that owns the output,
the source files that its actions build the output from, and all the source
files of the other targets that those actions use, and of their dependencies.

Some outputs must not be built after every change. Give such an output as a
[`DevOutput`](../../api/build/DevOutput) with `only`, a list of target patterns
or labels:

```python
DevInfo(
    server = server,
    outputs = {
        "exe": DevOutput(exe, only = []),
        "patch": DevOutput(patch, only = [ctx.attrs.lib.label]),
    },
)
```

`buck2 dev` builds an output with `only` again by itself only after a change to
the own source files of those targets, or to those targets. After any other
change that the output may be built from, to a file, a target or a `.bzl` file,
it _holds the output back_: it stops building it, also after changes inside its
`only`, and sends `stale` with the output's name and the changed files. The
other outputs are still built, from the same files. `only = []` means that
`buck2 dev` never builds the output by itself.

An output is held back until the server asks for it with `build`, and that
build succeeds. `buck2 dev` then sends `stale` again, without it. A server that
starts has no outputs, so none is held back for it.

In the example, a change to the crate `lib` builds `patch`, and `exe` is stale
from then on, which is what a server that loads patches into a running `exe`
expects. A change to any other crate makes `patch` stale too: the server asks
for both, and starts the app again.

Here nothing waits: [Changes that wait](#changes-that-wait) is about `notify`
sets. A change that holds an output back stops a build of it that is running,
like any change to a file that a build builds from.

A `DevInfo` with `watch` cannot have outputs with `only`.

## Watch sets

Each `DevWatch` is a set of source files, and what to do when one of them
changes.

| Argument  | Meaning                                                                                                                         |
| --------- | ------------------------------------------------------------------------------------------------------------------------------- |
| `targets` | Target patterns (`"root//lib/..."`) or labels (`ctx.label`, `dep.label`). Their own source files belong to the set, not those of their dependencies. `None` means all targets in the dependency graph. |
| `inputs`  | Artifacts, or anything else `cmd_args` takes, such as a `RunInfo`. The targets they are built from belong to the set, with their source files. See [What `inputs` claim](#what-inputs-claim). |
| `build`   | Names of `DevInfo.outputs` to build after a change. `buck2 dev` then sends the server a `built` message.                       |
| `restart` | Build the server again, stop it, and start it again, also when the server's command is the same. See [The server command](#the-server-command). |
| `notify`  | Tell the server about the change, and build nothing. See [Changes that wait](#changes-that-wait).                                |

A `DevWatch` may have both `targets` and `inputs`. Without either, it means the
rest of the dependency graph, like `targets = None`. A `DevWatch` with none of
`build`, `restart` and `notify` does nothing for its files but keep them from
the sets after it. `buck2 dev` still builds the server's command again after a
change to one of them, as after any change. `notify` cannot be used with
`build` or `restart`.

`targets` and `inputs` claim different files. Take a crate that a running
program loads a new build of. Its patch is built from the crate's own source
files and from the libraries it uses, but only a change to the crate's own
files can go into a patch. `targets = [crate.label]` claims those. In contrast,
`inputs = server` also claims the targets that the server depends on, which
keeps every file that the server is built from out of a later set.

For each file, the **first** `DevWatch` whose files include it decides. Put
the narrow sets first, and a `targets = None` set last for the rest of the
graph:

```python
watch = [
    # The server, and everything it is built from: a change to these must not
    # wait in the last set. `buck2 dev` restarts the server if it is new.
    DevWatch(inputs = server),
    # The app's own sources: build a new patch.
    DevWatch(targets = [ctx.label], build = ["patch"]),
    # Everything else: only tell the server.
    DevWatch(targets = None, notify = True),
]
```

`buck2 dev` does not watch or read files itself: the daemon's file watcher
reports to it the changes in the project that may matter to the target (see
[Limits](#limits)), and once the files stop changing,
`buck2 dev` asks the daemon what each changed file is: the set that claims it,
if one does. A set changed when one of its files did, or when its targets did
(see [Changes to the graph](#changes-to-the-graph)). A set claims only files of
the configured dependency graph of the `buck2 dev` target: `targets` match only
targets in that graph. Without a `targets = None` set, the files of targets
that no set matches are ignored.

A file that no set claims changes nothing, and neither does a file that is
created and removed again, such as the temporary files some editors make when
they save. A source file that is a symlink to another file in the project is
built from that file, so the set that claims the symlink claims that file too.

Saving a file without changing it counts as a change to its set: `buck2 dev`
does not know what the files were. It does what the file's set asks for: a
build, which builds nothing new, so that the server gets `built` with `changed`
false for each output, or a restart. The server's command is the same after
such a save, so without a `restart` set the server keeps running.

The file watcher must report changes as they happen. The `notify` watcher,
the default outside Meta, does; `buck2 dev` stops with an error if the
`file_watcher` in the `[buck2]` section of `.buckconfig` names one that does
not, such as `fs_hash_crawler`. Files that the project's ignore rules exclude,
and `buck-out`, are never reported.

If the file watcher misses changes, or the daemon restarts, `buck2 dev` cannot
tell which files changed. It then builds the server's command again, and
restarts the server if the command is new or if a set restarts. It builds what
every set that builds names, and, if there is a `notify` set, sends `stale`
with the project root (see [Changes that wait](#changes-that-wait)).

The daemon finds what the sets claim once, and again only when the graph
changes: a target, a build file or a `.bzl` file. A change to a source file is
looked up in what it found, so what a change costs does not grow with the
graph. What it finds is which set has which target: it lists no source files,
since the targets already have them.

### What `inputs` claim

A set claims the source files of `inputs` target by target, not file by file:

- Of the target that owns an input, the set claims the source files that the
  target's actions build the input from, and no others. So a rule that builds
  the server and other outputs in one target can give the server's source files
  to one set and leave the rest to another.
- What those actions take from other targets gives the set those targets and
  all their dependencies, with all of their source files, whether or not the
  input is built from each one.

So a set with `inputs` may claim a file that its input is not built from: a
source file of a dependency that only another output of that dependency uses.
A change to it then builds or restarts although nothing is new. That keeps
`buck2 dev` from walking the action graph below the input, which takes far
longer than walking the targets.

An action that a [dynamic action](dynamic_dependencies.md) of the owning target
creates is only known once the dynamic action has run, so finding the claims
builds what such dynamic actions need, with the files as they are then, and is
done again when what they read changes. If that build fails, the set gets the
source files of the owning target and of its dependencies instead.

### Changes to the graph

The daemon also compares the build files of the packages in the graph, the
`.bzl` files that they load, and the targets. When one of them changed,
`buck2 dev` analyses the target again, which builds the server.

- A changed build file, or a file added to or removed from a package, counts
  as a change to the sets whose targets it changed. The targets of a set are
  those its `targets` match, and the targets that its `inputs` are built from.
  Each target belongs to the first set that has it.
- A changed `.bzl` file may change what any rule does, so it counts as a
  change to every set.
- If the server's command changed, `buck2 dev` starts the server again, as
  after any change (see [The server command](#the-server-command)).

## Changes that wait

Buck builds from the files on disk. After a file of a `notify` set changes, any
build, even of an output that a `build` set names, would build that change too.
For some tools that is wrong. For example, a tool that loads new code into a
running program cannot load code built against a changed dependency.

So after a file of a `notify` set changes, `buck2 dev` builds nothing
automatically until the server asks for a build or a restart. It does not
analyse the target again either, because that builds the server, so it does
not find out whether the server's command is new. A build file
of a package with a target in a `notify` set waits in the same way. A `.bzl`
file can change any target, so when there is a `notify` set, a changed `.bzl`
file waits too. `buck2 dev` sends the server a `stale` message with the changed
files, and remembers the other changes.

The wait ends when the server asks for a build or a restart. That build uses
all the changes, and `buck2 dev` then sends `stale` with no files. Before the
build, `buck2 dev` builds the server's command with the changes. If the command
is new, it restarts the server instead of building for it, and the changes
still wait: the new server gets `stale` after `ready`, and asks again. If the
build fails, the changes still wait: the server has no build with them. Changing a
file back to what it was does not end the wait: `buck2 dev` does not know what
the files were.

A change that waits and breaks the analysis, such as a syntax error in a build
file whose change waits, waits all the same: the server gets `stale`, not
`failed`.

While the server is not running, nothing would ask for the build that ends the
wait. So then a change that builds or restarts starts the server, built from
the files as they are, and it gets `stale` after `ready`.

## The server protocol

`buck2 dev` starts the server in the project root, with the environment
variable `BUCK2_DEV_SOCKET` set to the path of a Unix socket. Python connects
with `socket.AF_UNIX`, and Node with `net.connect(path)`.

The server does not have to connect. A server that never connects is only
started, and started again when its command or a `restart` set changes, or
after it exits.
`buck2 dev` still builds what `build` sets name, but has no one to tell.

A server that connects sends `{"type": "hello", "version": 1}`. `buck2 dev`
answers with `ready`, and from then on writes messages to the connection and
reads requests from it, one JSON object per line. Because the address is in the
environment, a process that the server starts, such as the real program behind
a wrapper script, can connect instead of the server. If a second connection
sends `hello`, it takes over from the first.

`buck2 dev` may add new message types and new fields. A server must ignore
messages whose `type` it does not know, and fields it does not know.
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
keeps watching. The next change that builds or restarts starts the server
again, built from the files as they are then. So a server can also run to
completion on each change, such as a test runner. `buck2 dev` ends on Ctrl-C,
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
| `ready`         | `version`: 1, `outputs`: the names in `DevInfo.outputs`   | The server sent `hello`.                                                                                     |
| `building`      | `gen`, `outputs`: names, `files`: the changed files       | A build starts.                                                                                              |
| `built`         | `gen`, `outputs`: name to `{path, digest, changed}`, and `failed`: name to error if some outputs failed | A build of at least one output succeeded. `path` is absolute. `changed` is false when the output is the same as at the last build. The outputs in `failed` did not build. |
| `failed`        | `gen`, `error`                                            | A build of every output, or the analysis of the target, failed. For an action that failed, `error` has what the terminal shows for it: the command, its exit code, and the end of its output. |
| `cancelled`     | `gen`                                                     | A source file changed during the build, so it was stopped. The next build builds its outputs too.           |
| `stale`         | `files`: the changed files whose change waits. Without a watch list, also `outputs`: the names of the outputs that are held back, and `files` are the changes that hold them back | That list changes. An empty list means that nothing waits. After missed changes the project root is in the list: any such file may have changed. |
| `graph_changed` |                                                           | A build file, a `.bzl` file or a target changed, or an analysis that failed works again, and `buck2 dev` analysed the target again. |
| `shutdown`      |                                                           | `buck2 dev` is stopping the server. See [Stopping the server](#stopping-the-server).                         |

`gen` is a number that increases with each build. `building` and `built` for
the same build have the same `gen`. A
`building` is followed by one of `built`, `failed` and `cancelled`, unless
`buck2 dev` exits first. A server gets `built`, `failed` or `cancelled` for a
build only if it got that build's `building`: a server that connects while a
build runs hears nothing of that build, and asks for what it needs after
`ready`. A `failed` without a `building` is about the analysis of the target,
or the build of the server's command.
Messages
are sent only to a server that has sent `hello`. If a change waits when the
server sends `hello`, `ready` is followed by `stale`.

While the daemon builds, it watches the files that the build depends on: the
files of the sets that build or restart, and the build files and `.bzl` files
whose change does not wait (see [Changes that wait](#changes-that-wait)). If
one of them gets different contents, the daemon stops the build, and
`buck2 dev` sends `cancelled`, asks about the changes once the files stop
changing, and starts the next build, which builds the stopped build's outputs
too. Saving a file without changing it, a change to a file that the build does
not use, and a change that waits do not stop a build. Other
changes during a build, and changes while the daemon analyses the target or says
what changed, are asked about once that is done. If analysing fails,
`buck2 dev` sends `failed`, and analyses again after the next change, or before
the next build that the server asks for, which then gets `building` and
`failed`. The same errors again, after a change that did not touch what failed,
are not sent again. Once the analysis works again, the server gets
`graph_changed`.

### Requests from the server

| `type`    | Fields                    | Effect                                                                                          |
| --------- | ------------------------- | ----------------------------------------------------------------------------------------------- |
| `hello`   | `version`: 1              | Start talking to `buck2 dev`. It answers with `ready`. Send it before anything else.            |
| `build`   | `outputs`: a list of names | Build these outputs now, with all changes, including those that wait. Clears `stale` if the build succeeds. If changes waited and the server's command is new with them, `buck2 dev` restarts the server instead. |
| `restart` |                           | Build the server again and restart it. Clears `stale`. If the build fails, the old server keeps running until the next analysis restarts it. |

`buck2 dev` reads the connection all the time, also while it builds, so a
server never waits for it to read. Requests that arrive during a build are
handled after it.

A typical server sends `hello`, then `{"type": "build", "outputs": [...]}` when
it gets `ready`, and uses each `built` message.

## Other buck2 commands

Each build and each analysis that `buck2 dev` does is a separate, short daemon
command. Between them `buck2 dev` holds nothing in the daemon, so other
commands, such as `buck2 build` in another terminal, run as usual.

If another command starts while a `buck2 dev` command runs, and it sees
different files or a different configuration, it does not wait for that
command. It stops the `buck2 dev` command, and `buck2 dev` sends it again when
the other command lets it in.

If the daemon goes away, for example because a client of another buck2 version
restarted it, `buck2 dev` connects again, starting a daemon if needed, and
repeats the request.

## Limits

- `buck2 dev` takes one target.
- A change to a file in a package of the graph costs two short daemon commands,
  even if no set claims the file: one says what the file is, and one builds the
  server's command again. A server that keeps writing files in such a
  package keeps `buck2 dev` asking. Changes elsewhere cost nothing: the daemon
  does not report them to `buck2 dev`, except to build files and `.bzl` files
  of the graph, `PACKAGE` files, `.buckconfig` files, and the directories
  above the packages.
- When a target, a build file or a `.bzl` file of the graph changes, the daemon
  walks the graph of the target again to find what the sets claim.
- A file that a `.buckconfig` includes under another name is not reported, so
  a change to it is only seen with the next change that is.
- A `.buckconfig` change is not a change of any watch set, but `buck2 dev` asks
  the daemon about it as about any other file, and the daemon reads the
  configuration again. If that changes the targets of the graph, such as a
  source file that `read_config` names, it is a change to them, as in
  [Changes to the graph](#changes-to-the-graph). Other configuration changes
  are used by the next build.
- `buck2 dev` runs only on Unix so far.
