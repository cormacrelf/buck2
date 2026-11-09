# Hot patching a Rust binary with subsecond under `buck2 dev`

This example runs a Rust binary, and patches new code into it while it runs
with [subsecond](https://crates.io/crates/subsecond), the hot patching library
of Dioxus. Linux only.

```sh
buck2 dev //app:dev
```

The app prints a line every second. Edit `tick` in `app/src/lib.rs` and save:
the next line comes from the new code, and the count goes on from where it was.

You need `cc`, `clang++`, `nm` and `python3` on your `PATH`, and network access
for the crates from crates.io.

## How it works

`//app:app` is the crate that is patched. `//app:main` is the binary; it is
linked with `-Wl,--export-dynamic`, so its symbols are in its dynamic symbol
table. `//app:app` includes a module that `//app:slow` makes after a 10 second
sleep, so each build of the crate is slow: change `src/lib.rs` again during a
build, and `buck2 dev` cancels the build and starts a new one. `buck2 dev`
builds two outputs of `//app:dev`:

- `exe`: the binary.
- `patch`: the object files of `//app:app[static_pic]`, linked into a shared
  library. The library leaves the symbols of the other crates undefined; when
  the app loads it, the dynamic loader finds them in the binary.

The server, `server/server.py`, starts the binary and reads its symbols. For
each new `patch` it writes a subsecond jump table to the app's stdin: for each
function of the patch, its address in the binary and in the patch. The app
calls `subsecond::apply_patch`, and `HotFn::call` then runs the new code.

The patch does not depend on the binary, so building it does not build the
binary again. The jump table must use the binary that is running, so the server
makes it, not a build action.

The watch sets in `defs.bzl`:

1. A change to `server/server.py` restarts the server and the app.
2. A change to `//app:app` builds `patch`.
3. Any other change builds `exe`. The server starts the app again if the binary
   changed.

If a patch calls a function that the binary does not have, such as a generic
function with new type arguments, the server asks for `exe` and starts the app
again.

## The third-party crates

`third-party/BUCK` is made by [reindeer](https://github.com/facebookincubator/reindeer)
from `third-party/Cargo.toml`. After a change to that file, run
`reindeer buckify` in this directory.
