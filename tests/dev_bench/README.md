# Timing `buck2 dev`

`gen.py` makes a project of any size, and `bench.py` times `buck2 dev` on it: the start, a
change to a source file, and a change to a build file.

```sh
./bootstrap/buck2 build -m opt //:buck2 --show-full-output    # the buck2 to time
python3 tests/dev_bench/gen.py /tmp/dev_bench 2000 5 10   # 10k targets, 100k source files
python3 tests/dev_bench/bench.py <buck2> /tmp/dev_bench outputs
```

Without `-m opt` the buck2 that is built is not optimised, and takes about five times as long
for everything but a change to a source file.

The project is outside this repository, with a daemon of its own for each mode, so the run
does not disturb the daemon of this repository. `bench.py` stops its daemon when it is done.

The modes are targets of the project's `app` package, which differ in their outputs:

- `outputs`: a small output, and one that is built from every library.
- `outputs8`: the same, with eight outputs that are built from every library.

The project's server asks for every output when it starts, and for every output that
`buck2 dev` says is stale after a change.

Do not run `outputs` or `outputs8` without a limit on memory. In them the large output is built
after each change. Before the notify watcher left `buck-out` out, that build was stopped and
started again without end, and the daemon's memory grew until the machine ran out. They have
not been run since. See "a build that `buck2 dev` starts by itself can loop" in
`dev_review.md`.

The daemon logs how long each step of finding what the server and the outputs are built from
takes, at the `debug` level of `buck2_server_commands::dev`. `bench.py` turns that on with
`BUCK_LOG` and prints the steps at the end.
