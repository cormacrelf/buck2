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
server where it is.

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

`buck2 dev` watches only the files that a set claims, and a set claims only
files of the configured dependency graph of the `buck2 dev` target: `targets`
match only targets in that graph. Without a `targets = None` set, the files of
targets that no set matches are ignored.

To find the files of `inputs`, `buck2 dev` walks the action graph below them.
An action that a [dynamic action](dynamic_dependencies.md) creates is only
known once the dynamic action has run, so the walk builds what dynamic actions
need. If that build fails, the set gets the source files of the targets that
own those inputs and of their dependencies instead, and `buck2 dev` looks again
after the next change.

### Changes to the graph

`buck2 dev` also watches the build files of the packages in the graph, the
`.bzl` files that they load, and the package directories. When one of them
changes, or a file is added to or removed from a package, it analyses the
target again and finds the files of each set again.

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
  `buck2 dev` sends `stale` with no files, and builds the changes that waited.
- The server asks for a build or a restart. That build uses all the changes.

## The server protocol

`buck2 dev` starts the server in the project root. It writes messages to the
server's stdin, one JSON object per line. The server can write requests to its
stdout, one JSON object per line. `buck2 dev` prints other lines that the
server writes to stdout, and reports a JSON object with a `type` that is not a
valid request. The server's stderr goes to the terminal.

To stop the server, before a restart, `buck2 dev` closes the server's stdin and
waits up to 5 seconds for it to exit, then kills it. A server that starts
processes of its own should stop them when its stdin closes. When `buck2 dev`
itself exits, for example on Ctrl-C, it closes the server's stdin and sends it
`SIGTERM`.

### Messages to the server

| `type`          | Fields                                                    | Sent when                                                                                                    |
| --------------- | --------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| `ready`         | `outputs`: the names in `DevInfo.outputs`                 | The server has started, at the start and after each restart.                                                 |
| `building`      | `gen`, `outputs`: names, `files`: the changed files       | A build starts.                                                                                              |
| `built`         | `gen`, `outputs`: name to `{path, digest, changed}`       | A build succeeded. `path` is absolute. `changed` is false when the output is the same as at the last build. |
| `failed`        | `gen`, `error`                                            | A build, or the analysis of the target, failed. The errors are also on the terminal.                         |
| `cancelled`     | `gen`                                                     | A source file changed during the build, so `buck2 dev` stopped it. The next build builds its outputs too.   |
| `stale`         | `files`: the changed files of `notify` sets               | That list changes. An empty list means that nothing waits.                                                   |
| `graph_changed` |                                                           | A build file or a `.bzl` file changed, or a package got a new file, and `buck2 dev` analysed the target again. |

`gen` is a number that increases with each build. `building` and `built` for
the same build have the same `gen`. A
`building` is followed by one of `built`, `failed` and `cancelled`.

`buck2 dev` reads file events while it builds. When a file changes that makes
the build out of date, and whose change does not wait (see
[Changes that wait](#changes-that-wait)), it cancels the build, and starts a new
one once the files stop changing. It does the same when it analyses the target
again. If analysing fails, it sends `failed`, keeps watching the files it had,
and analyses again after the next change, or before the next build that the
server asks for.

### Requests from the server

| `type`    | Fields                    | Effect                                                                                          |
| --------- | ------------------------- | ----------------------------------------------------------------------------------------------- |
| `build`   | `outputs`: a list of names | Build these outputs now, with all changes, including those that wait. Clears `stale`.          |
| `restart` |                           | Build the server again and restart it. Clears `stale`. If the build fails, or a file change interrupts it, the old server keeps running until the next analysis restarts it. |

A typical server sends `{"type": "build", "outputs": [...]}` when it gets
`ready`, and uses each `built` message.

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
- A new directory inside a package is not watched until `buck2 dev` analyses
  the target again. That happens after a change to a build file or to a `.bzl`
  file, or when a file is added to a directory that is already watched.
- Changes to `.buckconfig` files are not watched. The next build uses them.
