// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runs the example project under a real Buck2.
//!
//! This is the half of the contract `cli.rs` cannot reach: that the rule
//! library builds, that Buck2 accepts the `InternalRunnerTestInfo` it returns,
//! and that the JSON the two commands write is what Buck2's callbacks parse
//! back out.
//!
//! It is opt-in. Buck2 needs a fix that is not upstream yet -- an
//! `InternalRunnerTestInfo` target makes `buck2 test` fail while tearing the
//! run down, and a failing test exits zero -- so running this against a stock
//! Buck2 reports a problem that is not nextest's. Set
//! `BUCK2_NEXTEST_RUN_EXAMPLE=1` once `buck2` on `PATH` carries the fix.

use camino::Utf8PathBuf;
use std::process::Command;

/// The environment variable that opts in to this test.
const OPT_IN: &str = "BUCK2_NEXTEST_RUN_EXAMPLE";

/// Returns the Buck2 project in this repository, or `None` to skip.
fn buck_project() -> Option<Utf8PathBuf> {
    if std::env::var_os(OPT_IN).is_none() {
        eprintln!("skipping: set {OPT_IN}=1 to run the example under Buck2");
        return None;
    }

    if Command::new("buck2").arg("--version").output().is_err() {
        eprintln!("skipping: `buck2` is not on PATH");
        return None;
    }

    let project = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("buck");
    assert!(project.is_dir(), "the example project is at {project}");
    Some(project)
}

/// Runs `buck2 test` in the example project with this binary on `PATH`.
fn buck2_test(project: &Utf8PathBuf, args: &[&str]) -> std::process::Output {
    // The toolchain finds `buck2-nextest` on PATH, so point at the one this
    // test was built alongside rather than whatever else is installed.
    let binary_dir = Utf8PathBuf::from(env!("CARGO_BIN_EXE_buck2-nextest"))
        .parent()
        .expect("the test binary has a parent directory")
        .to_owned();
    let path = match std::env::var("PATH") {
        Ok(existing) => format!("{binary_dir}:{existing}"),
        Err(_) => binary_dir.to_string(),
    };

    Command::new("buck2")
        .current_dir(project)
        .arg("test")
        .args(args)
        .env("PATH", path)
        .output()
        .expect("buck2 runs")
}

/// The example's tests all pass, bar the one that is ignored.
#[test]
fn the_example_project_passes() {
    let Some(project) = buck_project() else {
        return;
    };

    let output = buck2_test(&project, &["//example/..."]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "buck2 test succeeds, got {:?}:\n{stderr}",
        output.status.code()
    );

    // Buck2 renders one row per test rather than one per target, which is the
    // whole point of running them through the internal runner.
    assert!(
        stderr.contains("Pass 6"),
        "every test is reported individually:\n{stderr}"
    );

    // The ignored test is listed, then reported as skipped when Buck2 asks for
    // it -- rather than quietly missing from the run.
    assert!(
        stderr.contains("Skip 1"),
        "the ignored test is reported as skipped:\n{stderr}"
    );
}

/// Labels reach Buck2, so its own filtering still applies to these targets.
#[test]
fn buck2_can_filter_the_example_by_target() {
    let Some(project) = buck_project() else {
        return;
    };

    let output = buck2_test(&project, &["//example:demo-integration-test"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "buck2 test succeeds, got {:?}:\n{stderr}",
        output.status.code()
    );
    assert!(
        stderr.contains("Pass 3"),
        "only the integration test's tests ran:\n{stderr}"
    );
}
