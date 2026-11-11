# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# A `buck2 dev` server that does not connect. It appends `started`, with its process ID, its
# arguments and its environment, to `buck-out/dev_events.jsonl`, and runs until it is stopped.

import json
import os
import sys
import time

with open(os.path.join("buck-out", "dev_events.jsonl"), "a") as f:
    f.write(
        json.dumps(
            {
                "type": "started",
                "pid": os.getpid(),
                "argv": sys.argv[1:],
                "env": dict(os.environ),
            }
        )
        + "\n"
    )

while True:
    time.sleep(60)
