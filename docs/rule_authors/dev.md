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

- `server`: the command that `buck2 dev` starts once and keeps running.
- `outputs`: the artifacts that `buck2 dev` can build, by name.
- `watch`: a list of [`DevWatch`](../../api/build/DevWatch) values. They say
  what to do when a source file changes.
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
                DevWatch(inputs = server, restart = True),
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
`server.py` changes, it starts the server again. When any other source file in
the dependency graph of `//:site` changes, it builds `www` again and tells the
server where it is, if the server has connected to `buck2 dev` (see
[The server protocol](#the-server-protocol)).

[`examples/dev_web_server`](https://github.com/facebook/buck2/tree/main/examples/dev_web_server)
has a complete web server that serves the newest build.

## Watch sets

Each `DevWatch` is a set of source files, and what to do when one of them
changes.

| Argument  | Meaning                                                                                                                         |
| --------- | ------------------------------------------------------------------------------------------------------------------------------- |
| `targets` | Target patterns (`"root//lib/..."`) or labels (`ctx.label`, `dep.label`). Their own source files belong to the set, not those of their dependencies. `None` means all targets in the dependency graph. |
| `inputs`  | Artifacts, or anything else `cmd_args` takes, such as a `RunInfo`. The source files they are built from belong to the set, all the way down the action graph. |
| `build`   | Names of `DevInfo.outputs` to build after a change. `buck2 dev` then sends the server a `built` message.                       |
| `restart` | Build the server again, stop it, and start it again.                                                                             |
| `notify`  | Tell the server about the change, and build nothing. See [Changes that wait](#changes-that-wait).                                |

A `DevWatch` may have both `targets` and `inputs`. Without either, it means the
rest of the dependency graph, like `targets = None`. A `DevWatch` with none of
`build`, `restart` and `notify` makes `buck2 dev` ignore its files. `notify`
cannot be used with `build` or `restart`.

`targets` and `inputs` claim different files. Take a crate that a running
program loads a new build of. Its patch is built from the crate's own source
files and from the libraries it uses, but only a change to the crate's own
files can go into a patch. `targets = [crate.label]` claims those. In contrast,
a server is out of date when anything it is built from changes, so
`inputs = server` suits it.

For each file, the **first** `DevWatch` whose files include it decides. Put
the narrow sets first, and a `targets = None` set last for the rest of the
graph:

```python
watch = [
    # The server, and everything it is built from: restart it.
    DevWatch(inputs = server, restart = True),
    # The app's own sources: build a new patch.
    DevWatch(targets = [ctx.label], build = ["patch"]),
    # Everything else: only tell the server.
    DevWatch(targets = None, notify = True),
]
```

`buck2 dev` does not watch or read files itself: the daemon's file watcher
reports every change in the project to it, and once the files stop changing,
`buck2 dev` asks the daemon what the changes mean. The daemon finds the files
that each set claims, with the files as they are now, and says which sets
changed: those whose claimed files have different contents, or whose targets
changed. A set claims only files of the configured dependency graph of the
`buck2 dev` target: `targets` match only targets in that graph. Without a
`targets = None` set, the files of targets that no set matches are ignored.

Because the daemon compares contents, saving a file without changing it, a
file that is created and removed again, such as the temporary files some
editors make when they save, and a file that no set claims change nothing.

The file watcher must report changes as they happen. The `notify` watcher,
the default outside Meta, does; `buck2 dev` stops with an error if the
`file_watcher` in the `[buck2]` section of `.buckconfig` names one that does
not, such as `fs_hash_crawler`. Files that the project's ignore rules exclude,
and `buck-out`, are never reported. If the file watcher misses changes, or the
daemon restarts, `buck2 dev` asks the daemon in the same way: the daemon
compares contents, not events, so its answer says what changed all the same.

To find the files of `inputs`, the daemon walks the action graph below them,
each time `buck2 dev` asks. An action that a
[dynamic action](dynamic_dependencies.md) creates is only known once the
dynamic action has run, so the walk builds what dynamic actions need, with the
files as they are then. If that build fails, the set gets the source files of
the targets that own those inputs and of their dependencies instead.

### Changes to the graph

The daemon also compares the build files of the packages in the graph, the
`.bzl` files that they load, and the targets. When one of them changed,
`buck2 dev` analyses the target again, which builds the server.

- A changed build file, or a file added to or removed from a package, counts
  as a change to the sets whose targets it changed. The targets of a set are
  those its `targets` match, and the targets that own the actions below its
  `inputs`. Each target belongs to the first set that has it.
- A changed `.bzl` file may change what any rule does, so it counts as a
  change to every set.
- If the server's command line changed, `buck2 dev` starts the server again.

## Changes that wait

Buck builds from the files on disk. After a file of a `notify` set changes, any
build, even of an output that a `build` set names, would build that change too.
For some tools that is wrong. For example, a tool that loads new code into a
running program cannot load code built against a changed dependency.

So while a file of a `notify` set differs from what it was at the last build
that the server asked for, `buck2 dev` builds nothing automatically. It does
not analyse the target again either, because that builds the server. A build
file of a package with a target in a `notify` set waits in the same way. A
`.bzl` file can change any target, so when there is a `notify` set, a changed
`.bzl` file waits too. It sends
the server a `stale` message with the changed files, and remembers the other
changes. The changes wait until one of these happens:

- The files go back to their old content, for example when you undo the edit.
  `buck2 dev` sends `stale` with no files, and does what the sets ask for the
  changes that are left.
- The server asks for a build or a restart. That build uses all the changes.

The daemon says which files' changes wait, and their contents. Telling what
changed takes the daemon a walk of the graph with the changed files, as
analysing does, but it does not build the server or any output.

## The server protocol

`buck2 dev` starts the server in the project root, with the environment
variable `BUCK2_DEV_SOCKET` set to the path of a Unix socket. Python connects
with `socket.AF_UNIX`, and Node with `net.connect(path)`.

The server does not have to connect. A server that never connects is only
started, and started again when a `restart` set changes or after it exits.
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
build shows its progress there, and between builds one line says how many files
`buck2 dev` watches, whether the server runs, and how the last build went. The
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
| `built`         | `gen`, `outputs`: name to `{path, digest, changed}`       | A build succeeded. `path` is absolute. `changed` is false when the output is the same as at the last build. |
| `failed`        | `gen`, `error`                                            | A build, or the analysis of the target, failed. The errors are also on the terminal.                         |
| `cancelled`     | `gen`                                                     | A source file changed during the build, so it was stopped. The next build builds its outputs too.           |
| `stale`         | `files`: the changed files of `notify` sets               | That list changes. An empty list means that nothing waits.                                                   |
| `graph_changed` |                                                           | A build file, a `.bzl` file or a target changed, or an analysis that failed works again, and `buck2 dev` analysed the target again. |
| `shutdown`      |                                                           | `buck2 dev` is stopping the server. See [Stopping the server](#stopping-the-server).                         |

`gen` is a number that increases with each build. `building` and `built` for
the same build have the same `gen`. A
`building` is followed by one of `built`, `failed` and `cancelled`, unless
`buck2 dev` exits first.
Messages
are sent only to a server that has sent `hello`. If a file of a `notify` set
differs when the server sends `hello`, `ready` is followed by `stale`.

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
| `build`   | `outputs`: a list of names | Build these outputs now, with all changes, including those that wait. Clears `stale`.          |
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
- Every change in the project, even to a file that no set claims, costs a short
  daemon command, in which the daemon walks the graph of the target again. A
  server that keeps writing files in the project keeps `buck2 dev` asking.
- `buck2 dev` does not act on changes to `.buckconfig` files. The next build
  uses them.
- `buck2 dev` runs only on Unix so far.
