/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

/// NOTE: those structs don't need to maintain wire compatibility, we use them only with matching
/// writer & reader.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct MiniperfOutput {
    pub raw_exit_code: Result<i32, String>,

    /// Total instructions executed.
    pub user_instructions: MiniperfCounter,
    pub kernel_instructions: MiniperfCounter,
}

/// The fields here come straight out of `perf_event_open`. The count is
#[derive(serde::Serialize, serde::Deserialize)]
pub struct MiniperfCounter {
    /// Value of the counter.
    pub count: u64,
    /// Time this counter was active, in ns. Equals time_running unless multiplexing happens.
    pub time_enabled: u64,
    /// Time the measured processes were running, in ns.
    pub time_running: u64,
}

impl MiniperfCounter {
    /// Return the count, adjusted for multiplexing.
    pub fn adjusted_count(&self) -> u64 {
        if self.time_enabled == self.time_running {
            return self.count;
        }

        let ratio = self.time_running as f64 / self.time_enabled as f64;
        (self.count as f64 * ratio) as u64
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_adjusted_count() {
        assert_eq!(
            MiniperfCounter {
                count: 123,
                time_enabled: 100,
                time_running: 100
            }
            .adjusted_count(),
            123
        );

        assert_eq!(
            MiniperfCounter {
                count: 123,
                time_enabled: 50,
                time_running: 100
            }
            .adjusted_count(),
            246
        );
    }
}
