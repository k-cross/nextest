// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tests for the `demo` library, in a second binary.
//!
//! Having two test binaries is what makes `binary_id()` filtersets meaningful
//! in this example, and this one carries the environment and artifact plumbing
//! that the unit test binary does not.

#[test]
fn add_across_crates() {
    assert_eq!(demo::add(20, 22), 42);
}

/// Reads a file Buck2 built and pointed at through the environment.
///
/// This is the interesting one: it only passes if `buck2-nextest` forwarded
/// the target's `env` from the spec, Buck2 materialized the artifact that
/// environment variable names, and the test ran in the Buck2 project root.
#[test]
fn greeting_comes_from_buck2() {
    assert_eq!(demo::greeting(), "hello from buck2");
}

/// Checks the directories nextest names, from the position a test is in.
///
/// A test resolves neither of these against the project: it runs wherever Buck2
/// said, so a relative path here would point at whatever that happens to be.
/// Buck2 does not pass the project root to the executor, so this is also the
/// check that it was worked out correctly from what Buck2 does say.
#[test]
fn nextest_names_directories_absolutely() {
    for name in ["NEXTEST_WORKSPACE_ROOT", "CARGO_MANIFEST_DIR"] {
        let value = std::env::var(name).unwrap_or_else(|error| panic!("{name} is set: {error}"));
        let path = std::path::Path::new(&value);
        assert!(path.is_absolute(), "{name} is absolute, got `{value}`");
        // `.buckconfig` marks the example's project root, and `BUCK` the
        // package every target here lives in -- which is that same directory.
        assert!(
            path.join(".buckconfig").is_file() && path.join("BUCK").is_file(),
            "{name} names the project root, got `{value}`"
        );
    }
}
