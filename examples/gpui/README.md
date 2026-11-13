# Hot patching a gpui app under `buck2 dev`

This example runs a [gpui](https://crates.io/crates/gpui) window, and patches
new code into it while it runs, with subsecond, as `examples/subsecond` does.
Linux only.

```sh
echo mesa > .gpu_driver   # or nvidia; see below
direnv allow
buck2 dev dev.bxl:app -- --binary //app:main --lib //app:app
```

The window has a button and a count of its clicks. Edit `view` in
`app/src/lib.rs` and save: the window draws the new code, and the count stays.

## The dev shell

`flake.nix` has `clang`, `rustc`, `python3`, `watchman`, and the libraries
that gpui links and loads. gpui draws with Vulkan, and on a system other than
NixOS a binary built with Nix cannot load the system's Vulkan driver, so the
server starts the app with `nixVulkan`, from
[nixGL](https://github.com/nix-community/nixGL). `.gpu_driver`, which git
ignores, names the driver for it: `nvidia` or `mesa` (Intel and AMD). The
NVIDIA shell reads the version of the host's driver, so `.envrc` enters it with
`--impure`.

## How it works

`dev.bxl`, `server/server.py` and `toolchains/` are those of
`examples/subsecond`, whose README says how they work. The server here starts
the app with `--runner`, which `dev.bxl` sets to `nixVulkan`.

`Render::render` calls `view` through `subsecond::HotFn`, so a frame runs the
newest patch of it. A patch arrives on the app's stdin, on a thread of its own,
and the app applies it on gpui's main thread, between two frames, and then
redraws each window: gpui draws a window again only when something in it
changes.

The state of the window is in the `Counter` entity, which a patch does not
change, so it outlives the patch. Code that only runs once, such as `run`,
which opens the window, takes effect after a restart.

## The third-party crates

`third-party/BUCK` is made by [reindeer](https://github.com/facebookincubator/reindeer)
from `third-party/Cargo.toml`. After a change to that file, run
`reindeer buckify` in this directory.
