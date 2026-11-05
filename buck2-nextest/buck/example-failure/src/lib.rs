// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A target with a single test that always fails.
//!
//! This lives outside `//example/...` so that package's tests stay all-green; `buck2_example.rs`
//! runs this one on its own, to check what Buck2 reports for a real failure -- the summary line
//! and the process exit code -- end to end through `buck2-nextest`.

#[cfg(test)]
mod tests {
    #[test]
    fn always_fails() {
        assert_eq!(1, 2, "this test always fails, to exercise the failure path");
    }
}
