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

The server, `server/server.py`, asks for both when it starts. Buck builds a new
directory for each build of `app`, and Vite's file watcher does not follow a
directory that is replaced. So the server copies each new `app` into
`buck-out/dev_vite/app`, writes only the files whose content changed, and runs
Vite there. Vite sees the changed files and sends the updates to the browser.

The watch sets in `defs.bzl` say what each change does:

1. A change to `server/server.py` restarts the server, and so Vite.
2. A change to `node/package.json`, the lockfile or `pnpm-workspace.yaml`
   installs the packages again. The server restarts Vite on the new
   `node_modules`.
3. Any other change builds `app` again.

The server talks to `buck2 dev` over the socket in `BUCK2_DEV_SOCKET`. The
rule sets `DevInfo(interactive = True)`, so the server, and Vite, which it
starts, get the terminal, and Vite's keyboard shortcuts work: press `h` for the
list. Ctrl-C stops Vite and `buck2 dev`.
