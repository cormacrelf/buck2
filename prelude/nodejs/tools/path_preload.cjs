/**
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

// Loaded with `node --require` before a program. Like `pnpm run` with `node_modules/.bin`, it
// puts the directories in NODEJS_PREPEND_PATH, then the directory of this Node, at the front of
// PATH, so the program and the processes it starts find the bins of its dependencies and run
// `#!/usr/bin/env node` scripts with this Node.
"use strict";

const path = require("path");

const prepend = process.env.NODEJS_PREPEND_PATH;
delete process.env.NODEJS_PREPEND_PATH;
if (prepend) {
  // process.env is case-insensitive on Windows, so `PATH` also reaches `Path` there.
  process.env.PATH = [prepend, path.dirname(process.execPath), process.env.PATH]
    .filter(Boolean)
    .join(path.delimiter);
}
