/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

//! The part of the app that `buck2 dev` patches while it runs. Edit `tick` and save.

// Empty; its build step makes each build of this crate take 10 seconds. See `BUCK`.
mod slow;

use std::io::BufRead;
use std::time::Duration;

pub fn run() {
    // The server writes one jump table per line to our stdin.
    std::thread::spawn(|| {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            match serde_json::from_str::<subsecond::JumpTable>(&line) {
                // SAFETY: the server built the patch from this crate, against this binary.
                Ok(table) => match unsafe { subsecond::apply_patch(table) } {
                    Ok(()) => eprintln!("[app] patched"),
                    Err(e) => eprintln!("[app] could not apply the patch: {e}"),
                },
                Err(e) => eprintln!("[app] bad jump table: {e}"),
            }
        }
    });

    let mut count = 0u64;
    loop {
        count += 1;
        // Runs the newest patched version of `tick`. Not `subsecond::call(|| tick(count))`:
        // subsecond takes a closure of the size of a pointer for a function pointer.
        subsecond::HotFn::current(tick).call((count,));
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn tick(count: u64) {
    println!("tick {count}: hello from the first build");
}
