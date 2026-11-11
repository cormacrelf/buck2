# A dev server that builds ES modules on demand under `buck2 dev`

This example serves a page of ES modules, and builds each module only when the
browser asks for it. It needs `python3`, and nothing else: no network, and no
JavaScript toolchain.

```sh
buck2 dev dev.bxl:dev
```

Open <http://localhost:8000/>. To use another port, set `PORT`:
`PORT=9000 buck2 dev dev.bxl:dev`.

The page shows three greetings. Edit `lib/greet/names.js` and save: the page
reloads, and shows the new names.

## How it works

A browser dev server, such as Vite, does not know which modules a page needs
until the browser asks for them. `index.html` loads `/app/main.js`, which
imports `/lib/greet/greet.js`, which imports two more modules. So `DevInfo`
cannot list the modules as outputs: the server defines each one as an output
when it is first asked for, with a handler.

### The rule

`js_library` in `defs.bzl` compiles each source file with an action of its own,
and returns `JsModules`, a dictionary from the source file's path in the project
to the compiled module. The compiler, `tools/compile.py`, stands in for a real
one: it adds a comment that names the source file.

### The bxl functions

`dev.bxl` has two functions:

- `dev`, which `buck2 dev` runs. It returns a `DevInfo` with the server,
  `index.html` as the output `index`, and one handler, `module`.
- `module`, the handler. It takes `--path`, finds the target that owns the file,
  and returns its compiled module in `DefaultInfo`.

### The server

`server/server.py` serves `/` from `index`. For `GET /lib/greet/greet.js`, it
asks `buck2 dev` for the output `lib/greet/greet.js`. The first time, it defines
it:

```json
{"type": "build", "streaming": true, "outputs": {
  "lib/greet/greet.js": {"handler": "module", "args": ["--path", "lib/greet/greet.js"]}}}
```

Later it asks for it by name, with `null`. It waits for `built`, and serves the
file. Requests that arrive while a build runs go into the next build together.

`buck2 dev` finds what each module is built from, as for any output: here, its
source file and the compiler. After a change, `stale` names the modules that the
change may affect. The server drops those, and tells the open pages to reload,
on the event stream `/_events`. The reload asks for every module again; only
the stale ones are built.

A module that fails to build, such as one whose file does not exist, gets a 500,
and the server sends `forget` for it, so that `buck2 dev` stops tracking it.

## Files

- `defs.bzl`: the `js_library` rule.
- `dev.bxl`: the `dev` function and the `module` handler.
- `app/`, `lib/format/`, `lib/greet/`: three libraries of modules, and
  `index.html`.
- `server/server.py`: the dev server.
- `tools/compile.py`: the compiler.
- `toolchains/`: the prelude needs a `toolchains` cell. This example's rules do
  not use any toolchain.

See the rule author documentation page for `buck2 dev`, and its section on
handlers, for the messages between `buck2 dev` and the server.
