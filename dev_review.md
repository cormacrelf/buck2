I read this as a design, at the head of the stack, and did not run anything. The architecture (client owns the watcher and the server process, every build is a short preemptible daemon command) is right and I would keep it. The three things I would change before this hardens are the channel, the shape of `outputs`, and where file classification lives.

## Compared with what Buck already has

**`InstallInfo` is the closest precedent, not tests.** `InstallInfo(installer, files = {name: artifact})` plus `FileReady{name, digest, path, size}` is the same shape as `DevInfo(server, outputs)` plus `built`. Worth aligning names and fields with it deliberately, or saying why not.

**`outputs` is weaker than test commands.** A test's `command` and `env` are `cmd_args`, and `prepare_for_local_execution` materialises everything they reference and returns argv, env and cwd. `DevInfo.outputs` takes one bound artifact and rejects associated artifacts (`dev_info.rs` `AssociatedArtifacts`). An `exe` that needs shared libraries or resources cannot be expressed, so the subsecond example only works because it links statically. I would let outputs be `cmd_args`/`RunInfo` and have `built` return a resolved argv as well as a path.

**Local resources are missing.** `LocalResourceInfo` already covers "a helper process Buck starts, reads JSON from, and kills by pid" for simulators, emulators and databases. Dev servers need the same sidecars. `DevInfo(local_resources = {...})` with the env vars passed to the server would reuse it; today each server script has to do this itself.

**Results are all-or-nothing.** `build()` uses `try_compute_join`, so one failing output drops `built` for the others, and `failed.error` is error messages joined by newlines. `buck2 build` and the build report give per-target results and structured errors. A type-check-on-save server needs both.

## Subprocess communication

**Stdin and stdout are the wrong channel.** The test executor gets dedicated fds, and workers get a socket path in the environment. Here the protocol takes over stdio, with these costs:
- The server has no TTY, so Vite's hotkeys, `flutter run`'s `r`, and debuggers and REPLs can't work, even for a rule that wants them.
- Any child that inherits stdout can corrupt the protocol; the Vite example has to redirect `stdin=DEVNULL, stdout=sys.stderr`.
- Any stdout line that is JSON with a `type` is treated as a request.

Keeping JSON lines but moving them to a socket named in an env var fixes all three. It also makes the protocol opt-in, so a plain binary can be a server. See [Proposed subprocess design](#proposed-subprocess-design).

**Bugs in the current loop** (`app/buck2_client/src/commands/dev.rs`):
- **Blocking send:** `Server::send` awaits `write_all` inline (line 297). A server that doesn't read stdin fills the pipe and `buck2 dev` hangs, with nothing else polled.
- **Stdout stalls during builds:** `Session::request` (line 877) selects only on the daemon request and file events. Server output is not printed during a build, a server writing more than a pipe buffer blocks, and a server exit is not noticed until the build ends.
- **Crash ends the session:** a server exit, or a `Server::spawn` failure in `respawn`, exits `buck2 dev`. When the server is the app, a bad edit should mean "crashed, waiting for changes".
- **No process group:** `stop()` kills one pid, which is why the example needs `PR_SET_PDEATHSIG` through ctypes. `buck2_execute_local::process_group` and `kill_util::try_terminate_process_gracefully` already exist.
- **No wait on exit:** `Drop` sends SIGTERM and returns, so a quick rerun can hit a port still in use. On Ctrl-C the server gets SIGINT from the terminal, EOF and SIGTERM together.
- **Blocking hashing:** `hash_file` reads whole files synchronously on the async loop, including inside `interrupts()`.

Reader and writer tasks with channels, and a state machine that selects over everything, would fix the first three.

**The protocol has no version and no forward-compatibility rule.** `ready` should carry a version, and the docs should say servers must ignore unknown `type`s. Stop should be SIGTERM (or an explicit `shutdown` message), since real tools don't treat EOF on stdin as a stop.

### Proposed subprocess design

**Channel: an address in an environment variable, not an inherited fd.** `buck2 dev` listens before spawning and sets `BUCK2_DEV_SOCKET`:
- Unix: a Unix socket path under `buck-out`.
- Windows: a named pipe, `\\.\pipe\buck2-dev-<pid>-<random>`.

Both are a path the server opens: Python can `open()` a named pipe, and Node's `net.connect(path)` handles both kinds. Tokio has listeners for both. The wire format stays JSON lines.

Why not inherited fds:
- **Windows:** handle inheritance exists, but Rust's `Command` has no clean way to pass an extra inheritable handle, and the server has to turn a handle number back into something usable (`msvcrt.open_osfhandle` in Python; Node has no easy route). Buck's test executor avoids this: fds on Unix, TCP on Windows (`executor_launcher.rs`, `buck2_test/src/tcp`).
- **Unix:** fds don't survive wrappers. Python's `subprocess` closes fds above 2 by default, Node's `spawn` doesn't pass extras, and `npm run`/`sh -c` chains are common for dev servers.

Why not loopback TCP: any local process can connect, so it needs a token.

**Opt-in protocol.** A server that never connects is a plain "run and restart" process. One that connects sends `hello` with a protocol version and gets `ready` back. Because the address is a path, a grandchild (the real app behind a wrapper) can connect directly.

**I/O.** One task reads the connection into a channel and one writes from a channel. The state machine never blocks on the server, and selects over file events, daemon requests, server messages and child exit together.

**Lifecycle.**
- **Stop:** send `shutdown` if connected, then SIGTERM, wait up to the timeout, then kill the whole group. On Windows, use a Job object with kill-on-close and `CTRL_BREAK_EVENT` for the graceful step; `buck2_execute_local` already has process-group code for both platforms.
- **Exit:** do the same stop and wait for it, instead of signalling in `Drop` and returning.
- **Crash:** a non-zero exit prints "server exited, waiting for changes" and respawns on the next change. Only Ctrl-C or a clean exit ends the session.

**Terminal: background by default, foreground only when `DevInfo` asks.**

Default (`DevInfo` says nothing):
- The server gets its own process group and stays in the background.
- Its stdin is `/dev/null`. A background process that reads the terminal is stopped with SIGTTIN, so inheriting stdin here would hang servers that poll for keys.
- Its stdout and stderr are inherited.
- Ctrl-C reaches only `buck2 dev`, which runs the stop sequence, and the superconsole keeps its own key handling.

`DevInfo(interactive = True)`:
- The server's group becomes the terminal's foreground group (`tcsetpgrp`), and stdin is inherited.
- `buck2 dev` stops reading the terminal and drops to the simple console, since two programs drawing to one terminal will fight.
- A server killed by SIGINT means "quit", not "crashed".
- `buck2 dev` takes the foreground back around each restart and on exit, ignoring SIGTTOU while it does so.

On Windows there is no foreground group, so the flag reduces to whether stdin is inherited or `NUL`. The Job object handles cleanup either way.

The flag is named for what the server needs rather than `tui`: a REPL or a debugger prompt needs the same treatment. The interaction with the superconsole's key handling is unchecked and needs a prototype.

## Configuration

**`DevWatch` mixes two things: naming a file set and choosing an action.** I would name the sets (`watch = {"server": ..., "lib": ...}`) and include the set name in `building` and `stale`. The server currently gets raw paths and has to work out again what Buck already classified. The three flags, with `notify` exclusive and "none" meaning ignore, read better as one `action`.

**`targets` and `inputs` have different closure semantics behind similar names.** One is own sources, non-transitive; the other is the transitive action graph. Combined with first-match ordering and the implicit "neither means the rest", this is the part rule authors will get wrong. `targets` strings are also only parsed at the `Watch` request, so a bad pattern fails at runtime rather than at analysis.

**`notify` is a global mode, not a per-set action.** One pending file holds every build, every re-analysis and all `.bzl` changes. It accounts for most of the session state (`pending`, `held`, `held_graph_paths`, `baseline`, `resolve_due`, `graph_dirty`). It serves the hot-patch case only. I would make it a session-level hold the server controls (`hold`/`release` requests, or `DevInfo(auto_build = False)`), which removes the baseline hashing and the undo detection.

**There is no default.** Most uses are "run this and restart on change". `buck2 dev` on a target with only `RunInfo` should behave as `DevWatch(inputs = run, restart = True)`, and `buck2 dev //t -- args` should pass arguments. Neither is possible today, and there is no `env`.

## Will it extend?

| Kind | Verdict |
|---|---|
| Restart on change (air, nodemon, cargo-watch) | Works, but needs the side channel and crash tolerance to be clean |
| Static site with live reload | Works |
| Tools with their own watcher (Vite, webpack, Metro, `tsc --watch`) | Each server reimplements a sync to a stable directory (60 lines in the Vite example), because Buck replaces outputs wholesale |
| Hot patch | Works; it is what `notify` was built for |
| Mobile (simulator plus install) | Blocked on local resources and no link to `InstallInfo` |
| Multi-service (frontend, backend, database) | Blocked: one target, one server, no composition of `DevInfo`s |
| Test on save | Claimed in the doc, but there is no example and no link to `ExternalRunnerTestInfo`; I would drop the claim or prove it |
| Check on save with diagnostics | Needs per-output results and structured errors |

For the own-watcher row, Buck should own the sync: either a stable in-place sync target per output, or `changed_paths` for directory outputs, which the daemon can compute from directory digests.

## Where classification lives

The client reimplements the daemon's invalidation, and the "Limits" section lists where they disagree: new directories, `.buckconfig`, and probably `PACKAGE` files. It also does this with `notify` directly, ignoring the configured file watcher (watchman, Eden) and the project ignore rules.

Each re-resolve walks the whole configured graph and sends every source path as an absolute string, then the client adds one inotify watch per directory. That will not hold at monorepo scale.

The alternative is for the client to watch coarsely and send changed paths to a cheap `Classify{paths}` daemon command that returns the affected set names. That removes `DevWatched.files`, `targets_digest`, the `incomplete` fallback and the `Owner` map. It costs one round trip per batch of changes.

Two smaller questions on the daemon side:
- `Server`, `Watch` and `Build` are separate commands that reuse one cloned `ClientContext`, so they share a trace id. What do event logs and `buck2 log` look like after an hour of saves?
- `Server` then `Watch` always run back to back in `reresolve`; they could be one request.
