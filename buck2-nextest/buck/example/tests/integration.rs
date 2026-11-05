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
#[test]
fn nextest_names_directories_absolutely() {
    let workspace_root = absolute_var("NEXTEST_WORKSPACE_ROOT");
    let manifest_dir = absolute_var("CARGO_MANIFEST_DIR");

    assert!(
        workspace_root.join(".buckroot").is_file(),
        "NEXTEST_WORKSPACE_ROOT names the project root, got `{}`",
        workspace_root.display()
    );

    assert_eq!(
        manifest_dir, workspace_root,
        "CARGO_MANIFEST_DIR is where Buck2 ran the action"
    );
}

/// Reads an environment variable nextest sets, checking it is an absolute path.
fn absolute_var(name: &str) -> std::path::PathBuf {
    let value = std::env::var(name).unwrap_or_else(|error| panic!("{name} is set: {error}"));
    let path = std::path::PathBuf::from(&value);
    assert!(path.is_absolute(), "{name} is absolute, got `{value}`");
    path
}
