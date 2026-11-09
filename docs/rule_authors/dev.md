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

`buck2 dev` builds nothing on its own when it starts. The server asks for what
it needs.

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
            watch = [DevWatch(targets = None, build = ["www"])],
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

With this rule, `buck2 dev //:site` starts `python3 server.py`. When any source
file in the dependency graph of `//:site` changes, it builds `www` again and
tells the server where it is.

[`examples/dev_web_server`](https://github.com/facebook/buck2/tree/main/examples/dev_web_server)
has a complete web server that serves the newest build.

## Watch sets

Each `DevWatch` is a set of targets, and what to do when one of their source
files or build files changes.

| Argument  | Meaning                                                                                                                |
| --------- | ---------------------------------------------------------------------------------------------------------------------- |
| `targets` | Target patterns (`"root//lib/..."`) or labels (`ctx.label`, `dep.label`). `None` means all targets in the dependency graph. |
| `build`   | Names of `DevInfo.outputs` to build after a change. `buck2 dev` then sends the server a `built` message.              |
| `restart` | Build the server again, stop it, and start it again.                                                                    |
| `notify`  | Tell the server about the change, and build nothing. See [Changes that wait](#changes-that-wait).                       |

A `DevWatch` with none of `build`, `restart` and `notify` makes `buck2 dev`
ignore its files. `notify` cannot be used with `build` or `restart`.

For each file, the **first** `DevWatch` whose targets use the file decides. Put
the narrow sets first, and a `targets = None` set last for the rest of the
graph:

```python
watch = [
    # The app's own sources: build a new patch.
    DevWatch(targets = [ctx.label], build = ["patch"]),
    # The server's sources: restart it.
    DevWatch(targets = [ctx.attrs.server.label], restart = True),
    # Everything else: only tell the server.
    DevWatch(targets = None, notify = True),
]
```

`buck2 dev` watches the source files of the targets in the configured
dependency graph of the `buck2 dev` target, their build files, and the `.bzl`
files that those build files load. It ignores every other file.

## Changes that wait

Buck builds from the files on disk. After a file of a `notify` set changes, any
build, even of an output that a `build` set names, would build that change too.
For some tools that is wrong. For example, a tool that loads new code into a
running program cannot load code built against a changed dependency.

So while a file of a `notify` set differs from what it was at the last build
that the server asked for, `buck2 dev` builds nothing automatically. It sends
the server a `stale` message with the changed files, and remembers the other
changes. The changes wait until one of these happens:

- The files go back to their old content, for example when you undo the edit.
  `buck2 dev` sends `stale` with no files, and builds the changes that waited.
- The server asks for a build or a restart. That build uses all the changes.

## The server protocol

`buck2 dev` starts the server in the project root. It writes messages to the
server's stdin, one JSON object per line. The server can write requests to its
stdout, one JSON object per line. `buck2 dev` prints other lines that the
server writes to stdout. The server's stderr goes to the terminal.

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
| `stale`         | `files`: the changed files of `notify` sets               | That list changes. An empty list means that nothing waits.                                                   |
| `graph_changed` |                                                           | A build file or a `.bzl` file changed, or a package got a new file, and `buck2 dev` analysed the target again. |

`gen` is a number that increases with each build. `building` and `built` for
the same build have the same `gen`.

### Requests from the server

| `type`    | Fields                    | Effect                                                                                          |
| --------- | ------------------------- | ----------------------------------------------------------------------------------------------- |
| `build`   | `outputs`: a list of names | Build these outputs now, with all changes, including those that wait. Clears `stale`.          |
| `restart` |                           | Build the server again and restart it. Clears `stale`.                                          |

A typical server sends `{"type": "build", "outputs": [...]}` when it gets
`ready`, and uses each `built` message.

## Other buck2 commands

Each build that `buck2 dev` does is a separate, short daemon command. Between
builds `buck2 dev` holds nothing in the daemon, so other commands, such as
`buck2 build` in another terminal, run as usual.

If another command starts while a `buck2 dev` build runs, and it sees different
files or a different configuration, it does not wait for that build. It stops
the `buck2 dev` build, and `buck2 dev` starts the build again when the other
command lets it in.

## Limits

- `buck2 dev` takes one target.
- A new directory inside a package is not watched until `buck2 dev` analyses
  the target again. That happens after a change to a build file or to a `.bzl`
  file, or when a file is added to a directory that is already watched.
- Changes to `.buckconfig` files are not watched. The next build uses them.
