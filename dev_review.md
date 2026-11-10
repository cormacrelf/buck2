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

## Are the provider and the watch list the right shape?

This section comes from changing the code, not only from reading it. `buck2 dev` now builds the server's command again after every change it acts on, and restarts the server when the command line, or the contents of an artifact it names, is different. The examples dropped their restart sets. That is done; everything else below is a proposal.

**`server` and `outputs` are the right split, and the most important thing in the API.** What is in the server command restarts the server. What is in `outputs` is handed to a running server, which decides what a new build means: start the app again, load a patch, tell the browser. All three examples already work this way: the command names a script (and ThinLink in subsecond), and the app arrives as an output after `ready`. The API does not steer authors there, though. `server = binary[RunInfo]` is the obvious first thing to write, and it turns every reload into a restart without saying so. I would warn at analysis when an artifact in `outputs` is also named by the server command.

**A rule should say only what Buck cannot work out. `restart` failed that test.** Whether the server is out of date is a fact about its command, and Buck can check it exactly. As a rule:
- It was the same line, `DevWatch(inputs = server, restart = True)`, in every example.
- It restarted the server on a save that changed nothing.
- It could only be made exact by hashing every file of the set, which was removed because of what it cost on a large set.

Once Buck checked the command itself, the line went away in all three examples. `restart` is left for a server that reads files its command does not declare, which is a gap in the command more than a policy. I would keep it as an escape hatch and stop presenting it as the normal way.

**`build` mostly fails the same test.** A `build` set maps files to outputs, and the build graph already has that map. Where the author's map names fewer outputs than the graph does, the result is a stale output and no message. Claims are exclusive, so with `DevWatch(targets = ["//styles/..."], build = ["css"])` ahead of `DevWatch(targets = None, build = ["js"])`, a file under `//styles` that the JS bundle also reads builds only `css`. `js` stays old until some other change builds it. (This is from reading the claims code; I did not test it.) The author has to know the build graph to write a correct list, and nothing checks the list against it.

Where the author's map does carry information is exclusion. In subsecond, a change to the crate must build `patch` and must not build `exe`, though `exe` is built from the crate too. That is a real policy, and it needs a list. `notify` is the same kind of thing: a hold, not a trigger.

**So I would invert the default.** With no `watch`, `buck2 dev` builds every output after any change (or every output the server has asked for), and `changed` tells the server which are new. A build that has nothing to do is cheap: about 1ms on the 50k-target project with `-m opt`. `dev_web_server` and `dev_vite` would then need no watch list at all. The list stays for subtraction: "for these files, build only these outputs", and "hold". That also answers "There is no default" above.

**Automatic behaviour and first-match claims do not compose yet.** The server check is the first thing `buck2 dev` does that is not a rule, and it met the list in three places:
- **A server file that a `build` set claims.** Saving `server/server.py` unchanged in `dev_web_server` falls to `DevWatch(targets = None, build = ["www"])`, and the server is told of a build that built nothing. Observed by running the example.
- **A server file that a `notify` set claims.** The change waits, so the server is not checked until it asks for a build. The author has to put a set with no action first to keep the file out. So "no action" no longer means "ignore": it means "claim the file, and leave it to the server check".
- **A server file that no set claims.** It still has to be checked, so the check runs for every reported change, and each one now costs two daemon commands.

I would have the server command claim its own files ahead of the list. Only the precise part of the claim: the source files that the actions of the targets owning the command's artifacts read. That covers `server.py` in all three examples and `thinlink.rs` in subsecond. A save without a change then does nothing, a real change restarts, neither can wait behind `notify`, and the set with no action goes back to meaning "ignore".

**Over-claiming is only safe under an action that does everything.** Below the owning target, an `inputs` set claims whole targets and all their dependencies, whether or not the input is built from each file. Under `restart` that was harmless: the worst case was an extra restart, and the new server asked for everything again. Under `build`, or under the server check, an exclusive claim on a file the set is not built from takes the file away from the set that is. So this is why the implicit server claim above should not use the full `inputs` closure: a dependency shared by the server and an output would go to the server only, and when the server came out the same, the output would stay old. I would make the over-claimed part of any `inputs` claim non-exclusive, so that such a file also goes to the next set that matches it.

| Part | Verdict |
|---|---|
| `server` / `outputs` split | Keep, and make it the centre of the docs. Warn when an output is also in the server command |
| `restart` | An escape hatch for files the command does not declare, not the normal way |
| `build` sets | Default to every output; keep the list for building fewer |
| `notify` | A hold, as under Configuration |
| First-match exclusivity | Right for `targets`, wrong for the over-claimed part of `inputs` |
| A set with no action | Goes back to "ignore" once the server command claims its own files |

### Proposal: put the policy on the outputs, with `only`

The points above patch the watch list. This goes further and drops it. The list makes the author split the files into parts and give each part an action, but Buck already knows when the server is out of date and which outputs a file can affect. What the author knows is where to hold back. So the policy goes on the output, as one argument:

```python
# dev_web_server, and dev_vite with its two outputs: no rules at all
DevInfo(server = server, outputs = {"www": www})

# subsecond
DevInfo(
    server = cmd_args("python3", ctx.attrs.server, "--thinlink", thinlink),
    outputs = {
        "exe": DevOutput(exe, only = []),
        "patch": DevOutput(patch, only = [ctx.attrs.lib.label]),
    },
)
```

**How it behaves:**
- **The server** restarts when its command is new, as now. Its files are found from the command and trigger only that check. The `restart` rule goes; the `restart` request and `hidden` stay.
- **A plain output** is rebuilt after any change it may be built from. Which files those are comes from the walk that an `inputs` set does today, done once for each output.
- **Nothing is exclusive.** A file can belong to the server and to several outputs at once. Over-claiming then costs a build that does nothing, and can no longer leave an output stale. First-match order, "the rest of the graph" and the set with no action all go away.
- **`only = [targets]`**: the output is rebuilt by itself only after a change to the own source files of those targets. After any other change that it may be built from, `buck2 dev` stops rebuilding it and sends `stale` with the output's name, until the server asks for it. `only = []` means never by itself.
- **`notify` becomes that hold, for one output.** Today one file that waits stops every build and the analysis. Here only the output that the change affects waits, and the others build.

**Subsecond under it.** A change to the crate builds `patch`. A change to a dependency marks `patch` and `exe` stale; the server asks for `exe`, starts the app again, and then asks for `patch`, which lets patches build by themselves again. I think this is more correct than today's rules. As far as I can tell from reading them, a crate edit made while the slow `exe` build runs produces a patch built against the new dependency, and the server applies it to the old binary.

**A file added later** (see below) needs no rule here either: `only` names targets, so a new file that a target's glob takes in is in scope as soon as the target has it.

**What it costs, and what I am unsure of:**
- **The walks.** One for each output and one for the server at every graph change, where today it is one for each `inputs` set. They cover the same graph and should share work. Not measured.
- **Holds that were not needed.** Below the owning target, membership is still by whole target, so a hold can start for a file that the output is not built from. The server asks, and gets `changed: false`.
- **The protocol.** `stale` gets the output names, so every server that reads it changes.
- **"Ignore these files" cannot be said.** I do not think anything needs it once the server's files are found from its command, but I have not checked every fixture.
- **Not prototyped.** In particular, whether one output can be held while the others build from the same files is argued here, not shown.

### To think about: a file added after the sets were found

A rule says "changes to these files can be hot patched". Then a file appears that was not among them. What should happen, and do we test it?

**What a set can name.** `DevWatch` takes targets and artifacts, never paths or globs. So a new file belongs to a set only once a target takes it in:
- **The target's `srcs` is a glob that matches it.** The target changes, which is a change to the graph for the set that has the target: `buck2 dev` analyses again, the set acts, and the server gets `graph_changed`.
- **The target's `srcs` is a list.** The file belongs to no target, so nothing happens. Editing the build file to add it is the change.

So I think "these files" is the wrong way to think of a set, and the answer to "is that a bad use of the API" is yes. A rule that wants every file matching a glob should name a target whose sources are that glob: the library itself, or a filegroup. The docs should say this outright.

**What is tested.**
- One set: `test_new_source_file_changes_the_graph` (a glob takes the new file, `targets = None` builds) and `test_file_that_the_glob_does_not_match` (nothing happens).
- A narrow set that builds `patch`, then a catch-all `notify` set: `test_new_file_joins_the_inputs_set_of_its_target` (the narrow set is named by `inputs`) and `test_new_file_joins_the_targets_set_of_its_target` (by `targets`). In both, a new file that the glob matches sends `graph_changed`, builds, and does not wait; it is in `building.files`. A later change to it builds as any other file of the set does, and removing it builds again. Both pass, so the code already does the right thing here.

**What is still not tested, and partly not decided:**
1. A new file joins a target of the `notify` set. I expect `stale` with the file. It also changes the target, so does the rule that a build file "waits" apply to it?
2. A new file that the glob takes in but the input's actions do not read, in a target with several outputs. It falls to the next set. Is that what the author expects?
3. The server cannot tell what happened. `graph_changed` has no fields, and a hot-patching server needs to know that the patched crate gained a file, since some tools cannot patch that and must restart. This is another reason to name the sets and put the names in `graph_changed`.
4. A rename, which is a removal and an addition at once.

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
