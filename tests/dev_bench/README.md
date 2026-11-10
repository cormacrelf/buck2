# Timing `buck2 dev`

`gen.py` makes a project of any size, and `bench.py` times `buck2 dev` on it: the start, a
change to a source file, and a change to a build file.

```sh
./bootstrap/buck2 build -m opt //:buck2 --show-full-output    # the buck2 to time
python3 tests/dev_bench/gen.py /tmp/dev_bench 2000 5 10   # 10k targets, 100k source files
python3 tests/dev_bench/bench.py <buck2> /tmp/dev_bench inputs
```

Without `-m opt` the buck2 that is built is not optimised, and takes about five times as long
for everything but a change to a source file.

The project is outside this repository, with a daemon of its own for each mode, so the run
does not disturb the daemon of this repository. `bench.py` stops its daemon when it is done.

The modes are targets of the project's `app` package, which differ in their watch sets:

- `targets`: one `targets = None` set that builds a small output.
- `inputs`: before it, a set whose `inputs` are built from every library. It builds too.
- `restart`: the same, but the `inputs` set restarts the server, so `buck2 dev` compares the
  contents of its files, which are all of them.

The daemon logs how long each step of finding what the watch sets claim takes, at the `debug`
level of `buck2_server_commands::dev`. `bench.py` turns that on with `BUCK_LOG` and prints the
steps at the end.
