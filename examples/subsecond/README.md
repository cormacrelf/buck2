# Hot patching a Rust binary with subsecond under `buck2 dev`

This example runs a Rust binary, and patches new code into it while it runs
with [subsecond](https://crates.io/crates/subsecond), the hot patching library
of Dioxus. Linux only.

```sh
buck2 dev //app:dev
```

The app prints a line every second. Edit `tick` in `app/src/lib.rs` and save:
the next line comes from the new code, and the count goes on from where it was.

You need `clang`, `clang++`, `rustc` and `python3` on your `PATH`, and network
access for the crates from crates.io.

## How it works

`//app:app` is the crate that is patched, and `//app:main` is the binary.
`//app:app` includes a module that `//app:slow` makes after a 10 second sleep,
so each build of the crate is slow: change `src/lib.rs` again during a build,
and `buck2 dev` cancels the build and starts a new one.

### ThinLink

The linker of the toolchain is ThinLink, `toolchains/thinlink.rs`: a version of
the linker of the same name in the Dioxus CLI. It wraps `clang++`. rustc runs
it with the target's `env`, and `SUBSECOND_LINK` there sets what it does:

- `fat`, set by `//app:main`: link every object file of the dependencies, keep
  every section, and export every symbol. A patch can then call any function of
  the dependencies, including those the binary does not call.
- `thin`, set by `//app:app`, for `//app:app[cdylib]`: link only the crate's
  own code into a shared library. It leaves out the dependencies, the standard
  library and rustc's allocator shim; when the app loads the patch, the dynamic
  loader finds them in the binary. `-Clink-dead-code` makes rustc generate all
  of the crate's code, though nothing in the library uses it.

ThinLink is built with `rustc` alone, by a `genrule`: a `rust_binary` would
need the toolchain that ThinLink is part of.

`buck2 dev` builds two outputs of `//app:dev`:

- `exe`: the binary.
- `patch`: `//app:app[cdylib]`, linked by ThinLink.

The server, `server/server.py`, starts a copy of the binary. For each new
`patch`, it runs `thinlink jump-table` on the binary and the patch, and writes
the subsecond jump table to the app's stdin: for each function of the patch, its
address in the binary and in the patch. The app calls `subsecond::apply_patch`,
and `HotFn::call` then runs the new code.

The patch does not depend on the binary, so building it does not build the
binary again. The jump table must use the binary that is running, so the server
makes it, not a build action.

The watch sets in `defs.bzl`:

1. A change to `server/server.py` restarts the server and the app.
2. A change to `//app:app` builds `patch`.
3. Any other change builds `exe`. The server starts the app again if the binary
   changed.

If a patch calls a function that the binary does not have, such as a function
of the standard library that the binary does not use, or a generic function with
new type arguments, the server asks for `exe` and starts the app again.

## The third-party crates

`third-party/BUCK` is made by [reindeer](https://github.com/facebookincubator/reindeer)
from `third-party/Cargo.toml`. After a change to that file, run
`reindeer buckify` in this directory.
