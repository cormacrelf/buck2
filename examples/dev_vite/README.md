# Vite with hot module replacement under `buck2 dev`

This example runs the [Vite](https://vite.dev) dev server on files that buck2
builds. One of the app's modules, `src/messages.js`, is made by a build step
from `app/data/messages.json`. Edit that JSON file, or `app/src/style.css`, and
the open page changes without a reload: Vite's hot module replacement applies
the new module, and the page keeps its state (the click count).

```sh
buck2 dev //app:app
```

Open <http://localhost:5173/>. To use another port, set `PORT`.

You need `pnpm` and `node` on your `PATH`, and network access: the
`//node:packages` target runs `pnpm install` in a local build action. That is a
quick way to get `node_modules` into buck-out, not a model for a real
JavaScript build.

## How it works

`buck2 dev` builds two outputs of `//app:app`:

- `packages`: the package directory with `node_modules`, from `pnpm install`.
- `app`: the app's files. The sources and the generated `src/messages.js` are
  together in one directory.

The server, `server/server.py`, asks for both when it starts, and for each one
that `buck2 dev` says is stale after a change. Buck builds a new
directory for each build of `app`, and Vite's file watcher does not follow a
directory that is replaced. So the server keeps the app's files in
`buck-out/dev_vite/app`, and runs Vite there.

The server does not compare the new directory with the old one to find what to
write. In `defs.bzl`, each file of the app is also a sidecar of the `app`
output, named by its path in the app:

```python
outputs = {"app": DevOutput(app, sidecars = files), "packages": packages}
```

`built` then has the path of each file, and whether it changed since the last
build. The server writes the changed ones. Vite sees them and sends the updates
to the browser.

The server's command names only `server/server.py`, so `buck2 dev` restarts
the server, and so Vite, only when that file gets new contents. For the other
changes, `buck2 dev` finds what each output may be built from:

1. A change to `node/package.json`, the lockfile or `pnpm-workspace.yaml` makes
   `packages` stale. The server asks for it, which installs the packages again,
   and restarts Vite on the new `node_modules`.
2. A change to a file of the app makes `app` stale, and the server asks for it.

The server talks to `buck2 dev` over the socket in `BUCK2_DEV_SOCKET`. The
rule sets `DevInfo(interactive = True)`, so the server, and Vite, which it
starts, get the terminal, and Vite's keyboard shortcuts work: press `h` for the
list. Ctrl-C stops Vite and `buck2 dev`.
