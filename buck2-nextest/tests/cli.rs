// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! End-to-end tests for the two commands Buck2 runs.
//!
//! These drive the real binary against a real libtest harness, because what is
//! worth checking is the whole path: the pipeline listing a binary, the runner
//! executing one of its tests, and the JSON Buck2's Starlark callbacks read
//! coming out the far end. Buck2 itself is not involved -- these assert the
//! half of the contract that lives in this crate, and `buck2_example.rs` covers
//! the other half.

use camino::{Utf8Path, Utf8PathBuf};
use camino_tempfile::Utf8TempDir;
use serde_json::{Value, json};
use std::{
    process::{Command, Output},
    sync::OnceLock,
};

/// A libtest harness with one test of each interesting shape.
///
/// `flaky` fails until its marker file exists, so it passes on a retry and only
/// on a retry.
static HARNESS_SOURCE: &str = r#"
pub fn add(a: i32, b: i32) -> i32 { a + b }

#[cfg(test)]
mod tests {
    #[test]
    fn passes() { assert_eq!(super::add(2, 2), 4); }

    #[test]
    fn fails() { panic!("the distinctive panic message"); }

    #[test]
    #[ignore]
    fn is_ignored() {}

    #[test]
    fn reads_target_env() {
        assert_eq!(
            std::env::var("BUCK2_NEXTEST_TARGET_ENV").as_deref(),
            Ok("from=the=target"),
        );
    }

    #[test]
    fn flaky() {
        let marker = std::env::var("BUCK2_NEXTEST_FLAKY_MARKER")
            .expect("the marker path is set");
        if std::path::Path::new(&marker).exists() {
            return;
        }
        std::fs::write(&marker, "1").expect("the marker is written");
        panic!("failing until the marker exists");
    }
}
"#;

/// A Buck2 project root holding a compiled harness.
///
/// Each test gets its own, so that tests running concurrently cannot race over
/// the compiled binary or a marker file.
struct Fixture {
    /// Kept so the directory outlives the test.
    _dir: Utf8TempDir,
    root: Utf8PathBuf,
    program: Utf8PathBuf,
    /// The default listing, which costs two executions of the harness to
    /// produce, so tests that ask for several filters produce it once.
    default_listing: OnceLock<Vec<Value>>,
}

impl Fixture {
    /// Builds a project root whose `.config/nextest.toml` is `config`.
    fn new(config: &str) -> Self {
        let dir = Utf8TempDir::new().expect("a temporary directory");
        let root = dir.path().to_owned();

        std::fs::write(root.join(".buckroot"), "").expect("the buckroot is written");
        std::fs::create_dir_all(root.join(".config")).expect("the config directory is created");
        std::fs::write(root.join(".config/nextest.toml"), config).expect("the config is written");

        let source = root.join("harness.rs");
        std::fs::write(&source, HARNESS_SOURCE).expect("the harness source is written");

        let program = root.join(format!("harness{}", std::env::consts::EXE_SUFFIX));
        let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
        let output = Command::new(rustc)
            .args(["--edition", "2021", "--test"])
            .arg(&source)
            .arg("-o")
            .arg(&program)
            .output()
            .expect("rustc runs");
        assert!(
            output.status.success(),
            "the harness compiles: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        Self {
            _dir: dir,
            root,
            program,
            default_listing: OnceLock::new(),
        }
    }

    /// Runs the binary in this project root.
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_buck2-nextest"))
            .current_dir(&self.root)
            .args(args)
            .args(["--label", LABEL, "--program", self.program.as_str()])
            .env_remove("NEXTEST_PROFILE")
            .env("BUCK2_NEXTEST_FLAKY_MARKER", self.root.join("flaky.marker"))
            .output()
            .expect("buck2-nextest runs")
    }

    /// Lists the harness, returning the parsed JSON.
    fn list(&self, args: &[&str]) -> Vec<Value> {
        let output = self.run(&[&["list"], args].concat());
        assert!(
            output.status.success(),
            "listing succeeds: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        parse(&output)
    }

    /// Uses the default profile, so tests a profile's default filter drops can
    /// still be asked for.
    fn listed_filter(&self, test_name: &str) -> String {
        self.default_listing
            .get_or_init(|| self.list(&[]))
            .iter()
            .find(|test| filter_name(test) == test_name)
            .map(|test| string(test, "filter").to_owned())
            .unwrap_or_else(|| panic!("`{test_name}` is listed"))
    }

    /// Runs one test the way Buck2 does, via the listing's `filter`.
    fn run_test(&self, test_name: &str, args: &[&str]) -> (Value, i32) {
        let filter = self.listed_filter(test_name);
        self.run_with_listed_filter(&filter, args)
    }

    fn run_with_listed_filter(&self, filter: &str, args: &[&str]) -> (Value, i32) {
        single_result(&self.run(&[&["run"], args, &["--listed-test", filter]].concat()))
    }

    fn run_named_test(&self, test_name: &str, args: &[&str]) -> (Value, i32) {
        single_result(&self.run(&[&["run"], args, &["--test-name", test_name]].concat()))
    }

    /// Wraps the harness in a script that logs each execution, for
    /// [`Self::take_invocations`].
    #[cfg(unix)]
    fn log_invocations(&mut self) {
        use std::os::unix::fs::PermissionsExt;

        let script = self.root.join("logged-harness");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
                self.invocation_log(),
                self.program,
            ),
        )
        .expect("the script is written");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("the script is made executable");
        self.program = script;
        // Anything listed before now was listed without the wrapper.
        self.default_listing = OnceLock::new();
    }

    #[cfg(unix)]
    fn invocation_log(&self) -> Utf8PathBuf {
        self.root.join("invocations.log")
    }

    /// Returns each logged execution's arguments, and clears the log.
    #[cfg(unix)]
    fn take_invocations(&self) -> Vec<String> {
        let log = self.invocation_log();
        let invocations = match std::fs::read_to_string(&log) {
            Ok(contents) => contents.lines().map(str::to_owned).collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("the invocation log is readable: {error}"),
        };
        if !invocations.is_empty() {
            std::fs::remove_file(&log).expect("the invocation log is cleared");
        }
        invocations
    }
}

/// The label the fixture is always run under.
const LABEL: &str = "root//app:harness";

/// The default configuration: no retries, so a flaky test simply fails.
const PLAIN_CONFIG: &str = "[profile.default]\n";

fn parse(output: &Output) -> Vec<Value> {
    let stdout = String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8");
    serde_json::from_str(&stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is the JSON Buck2 parses: {error}\nstdout was: {stdout}\nstderr was: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn single_result(output: &Output) -> (Value, i32) {
    let mut results = parse(output);
    assert_eq!(
        results.len(),
        1,
        "exactly one result: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (results.remove(0), exit_code(output))
}

fn exit_code(output: &Output) -> i32 {
    output.status.code().expect("the process was not signalled")
}

fn field<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    value.get(name)
}

fn string<'a>(value: &'a Value, name: &str) -> &'a str {
    field(value, name)
        .unwrap_or_else(|| panic!("`{name}` is present in {value}"))
        .as_str()
        .unwrap_or_else(|| panic!("`{name}` is a string in {value}"))
}

fn decoded_filter(test: &Value) -> Value {
    let filter = string(test, "filter");
    serde_json::from_str(filter)
        .unwrap_or_else(|error| panic!("the filter `{filter}` is JSON: {error}"))
}

fn filter_name(test: &Value) -> String {
    string(&decoded_filter(test), "name").to_owned()
}

/// Everything the harness has, ignored tests included, each with a filter that
/// describes the test.
#[test]
fn lists_every_test_including_ignored_ones() {
    let fixture = Fixture::new(PLAIN_CONFIG);
    let listed = fixture.list(&[]);

    let entries: Vec<(&str, Value)> = listed
        .iter()
        .map(|test| (string(test, "name"), decoded_filter(test)))
        .collect();

    let entry = |test_name: &str, ignored: bool| {
        (
            format!("root//app:harness - {test_name}"),
            json!({"name": test_name, "kind": "test", "ignored": ignored}),
        )
    };
    let expected = [
        entry("tests::fails", false),
        entry("tests::flaky", false),
        entry("tests::is_ignored", true),
        entry("tests::passes", false),
        entry("tests::reads_target_env", false),
    ];
    let expected: Vec<(&str, Value)> = expected
        .iter()
        .map(|(name, filter)| (name.as_str(), filter.clone()))
        .collect();

    assert_eq!(entries, expected);
}

#[test]
fn only_ignored_tests_are_listed_as_skipped_with_the_message_run_reports() {
    let fixture = Fixture::new(PLAIN_CONFIG);
    let listed = fixture.list(&[]);
    let (run_result, _) = fixture.run_test("tests::is_ignored", &[]);

    let preset_results: Vec<(String, Option<&Value>, Option<&Value>)> = listed
        .iter()
        .map(|test| {
            (
                filter_name(test),
                field(test, "status"),
                field(test, "message"),
            )
        })
        .collect();
    let skipped = json!("SKIP");
    let expected: Vec<(String, Option<&Value>, Option<&Value>)> = [
        ("tests::fails", None, None),
        ("tests::flaky", None, None),
        (
            "tests::is_ignored",
            Some(&skipped),
            field(&run_result, "message"),
        ),
        ("tests::passes", None, None),
        ("tests::reads_target_env", None, None),
    ]
    .into_iter()
    .map(|(name, status, message)| (name.to_owned(), status, message))
    .collect();

    assert_eq!(preset_results, expected);
    assert!(
        field(&run_result, "message").is_some(),
        "run reports a message for an ignored test: {run_result}"
    );
}

/// The profile's default-filter shapes what Buck2 is told exists, so it never
/// schedules an action for a test that would only be discarded.
#[test]
fn listing_honours_the_profiles_default_filter() {
    let fixture = Fixture::new(
        "[profile.narrowed]\ndefault-filter = 'test(=tests::passes)'\n\
         [profile.default]\n",
    );
    let listed = fixture.list(&["-P", "narrowed"]);

    let names: Vec<String> = listed.iter().map(filter_name).collect();
    assert_eq!(names, vec!["tests::passes"]);
}

#[test]
fn runs_a_passing_test() {
    let fixture = Fixture::new(PLAIN_CONFIG);
    let (result, code) = fixture.run_test("tests::passes", &[]);

    assert_eq!(string(&result, "name"), "root//app:harness - tests::passes");
    assert_eq!(string(&result, "status"), "PASS");
    assert_eq!(code, 0);
    assert!(
        field(&result, "duration").is_some_and(Value::is_number),
        "a passing test reports how long it took: {result}"
    );
    assert!(
        field(&result, "details").is_none(),
        "a passing test carries no diagnostic output: {result}"
    );
}

#[test]
fn reports_a_failing_test_with_its_panic_message() {
    let fixture = Fixture::new(PLAIN_CONFIG);
    let (result, code) = fixture.run_test("tests::fails", &[]);

    assert_eq!(string(&result, "name"), "root//app:harness - tests::fails");
    assert_eq!(string(&result, "status"), "FAIL");
    assert_eq!(code, 100);
    assert!(
        string(&result, "details").contains("the distinctive panic message"),
        "the details carry the test's own output: {result}"
    );
}

/// An ignored test is listed, so Buck2 asks about it and has to be told
/// something. Being skipped is not a failure, so the exit code must not say it
/// is one -- Buck2 synthesizes a status from the exit code whenever the JSON
/// does not parse.
#[test]
fn reports_an_ignored_test_as_skipped() {
    let fixture = Fixture::new(PLAIN_CONFIG);
    let (result, code) = fixture.run_test("tests::is_ignored", &[]);

    assert_eq!(
        string(&result, "name"),
        "root//app:harness - tests::is_ignored"
    );
    assert_eq!(string(&result, "status"), "SKIP");
    assert_eq!(code, 0, "a skipped test did not fail: {result}");
}

/// Naming a test by hand lists the whole binary and reports every other test as
/// skipped, so the result must be matched to the test asked for.
#[test]
fn reports_the_test_that_was_asked_for() {
    let fixture = Fixture::new(PLAIN_CONFIG);

    for test_name in ["tests::passes", "tests::fails", "tests::is_ignored"] {
        for (how, (result, _)) in [
            ("listed", fixture.run_test(test_name, &[])),
            ("named", fixture.run_named_test(test_name, &[])),
        ] {
            assert_eq!(
                string(&result, "name"),
                format!("root//app:harness - {test_name}"),
                "asked about {test_name} by being {how}"
            );
        }
    }
}

/// A failure's message is not compared, since a panic message includes the
/// thread ID.
#[test]
fn a_named_test_reports_what_a_listed_test_does() {
    let fixture = Fixture::new(PLAIN_CONFIG);

    for (test_name, keys) in [
        ("tests::passes", &["name", "status", "message"][..]),
        ("tests::fails", &["name", "status"][..]),
        ("tests::is_ignored", &["name", "status", "message"][..]),
    ] {
        let (listed, listed_code) = fixture.run_test(test_name, &[]);
        let (named, named_code) = fixture.run_named_test(test_name, &[]);
        for key in keys {
            assert_eq!(
                field(&listed, key),
                field(&named, key),
                "{test_name} reports the same {key}"
            );
        }
        assert_eq!(listed_code, named_code, "{test_name}");
    }
}

/// Nothing is written, so Buck2 would synthesize a failure from the exit code.
#[test]
fn an_unknown_test_name_is_an_error() {
    let fixture = Fixture::new(PLAIN_CONFIG);
    let output = fixture.run(&["run", "--test-name", "tests::does_not_exist"]);

    assert!(output.stdout.is_empty(), "nothing is reported to Buck2");
    assert_ne!(exit_code(&output), 0);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("tests::does_not_exist"),
        "the error names the test that was asked for: {stderr}"
    );
}

/// Checking would mean listing the binary again, and libtest runs nothing and
/// succeeds for an unknown name. Buck2 lists and runs the same binary, so in
/// practice the two agree.
#[test]
fn a_listed_test_is_trusted_to_exist() {
    let fixture = Fixture::new(PLAIN_CONFIG);
    let filter = json!({"name": "tests::does_not_exist", "kind": "test", "ignored": false});
    let (result, code) = fixture.run_with_listed_filter(&filter.to_string(), &[]);

    assert_eq!(
        string(&result, "name"),
        "root//app:harness - tests::does_not_exist"
    );
    assert_eq!(string(&result, "status"), "PASS");
    assert_eq!(code, 0);
}

#[test]
fn a_listed_test_must_come_from_the_listing() {
    let fixture = Fixture::new(PLAIN_CONFIG);
    let output = fixture.run(&["run", "--listed-test", "tests::passes"]);

    assert!(output.stdout.is_empty(), "nothing is reported to Buck2");
    assert_ne!(exit_code(&output), 0);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not a filter written by `buck2-nextest list`"),
        "the error says what the value should have been: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn a_listed_test_executes_the_binary_once() {
    let mut fixture = Fixture::new(PLAIN_CONFIG);
    fixture.log_invocations();
    let filter = fixture.listed_filter("tests::passes");
    assert_listing_logged(&fixture.take_invocations());

    let (result, code) = fixture.run_with_listed_filter(&filter, &[]);
    assert_eq!(string(&result, "status"), "PASS");
    assert_eq!(code, 0);

    let invocations = fixture.take_invocations();
    assert_eq!(invocations.len(), 1, "one execution: {invocations:?}");
    assert!(
        invocations[0].starts_with("--exact tests::passes "),
        "the one execution runs the test: {invocations:?}"
    );
}

#[cfg(unix)]
#[test]
fn an_ignored_listed_test_executes_nothing() {
    let mut fixture = Fixture::new(PLAIN_CONFIG);
    fixture.log_invocations();
    let filter = fixture.listed_filter("tests::is_ignored");
    assert_listing_logged(&fixture.take_invocations());

    let (result, code) = fixture.run_with_listed_filter(&filter, &[]);
    assert_eq!(string(&result, "status"), "SKIP");
    assert_eq!(code, 0);
    assert_eq!(fixture.take_invocations(), Vec::<String>::new());
}

/// Guards the tests above against a wrapper that logs nothing.
#[cfg(unix)]
fn assert_listing_logged(invocations: &[String]) {
    assert_eq!(
        invocations.len(),
        2,
        "listing runs the binary with and without --ignored: {invocations:?}"
    );
    assert!(
        invocations.iter().all(|args| args.contains("--list")),
        "the listing only lists: {invocations:?}"
    );
}

/// The contrast that makes the previous two tests meaningful.
#[cfg(unix)]
#[test]
fn a_named_test_lists_the_binary_first() {
    let mut fixture = Fixture::new(PLAIN_CONFIG);
    fixture.log_invocations();

    let (result, _) = fixture.run_named_test("tests::passes", &[]);
    assert_eq!(string(&result, "status"), "PASS");

    let invocations = fixture.take_invocations();
    let listings = invocations
        .iter()
        .filter(|args| args.contains("--list"))
        .count();
    assert_eq!(listings, 2, "the binary is listed first: {invocations:?}");
    assert_eq!(invocations.len(), 3, "and then run once: {invocations:?}");
}

/// Retries are nextest's, and they happen inside the single action Buck2 ran,
/// so a flaky test is reported to Buck2 as the pass it eventually was.
#[test]
fn retries_make_a_flaky_test_pass() {
    let fixture = Fixture::new("[profile.retried]\nretries = 2\n[profile.default]\n");
    let (result, code) = fixture.run_test("tests::flaky", &["-P", "retried"]);

    assert_eq!(string(&result, "status"), "PASS");
    assert_eq!(code, 0);
    assert!(
        string(&result, "message").contains("flaky"),
        "a pass that needed retries says so: {result}"
    );
}

/// A profile that fails flaky tests reports the FAIL Buck2 will act on, and a
/// message that says why it failed rather than one describing the pass.
#[test]
fn a_flaky_test_fails_when_the_profile_says_so() {
    let fixture =
        Fixture::new("[profile.strict]\nretries = 2\nflaky-result = 'fail'\n[profile.default]\n");
    let (result, code) = fixture.run_test("tests::flaky", &["-P", "strict"]);

    assert_eq!(string(&result, "status"), "FAIL");
    assert_ne!(code, 0);
    let message = string(&result, "message");
    assert!(
        message.contains("configured to fail when flaky"),
        "the message explains the failure rather than claiming a pass: {result}"
    );
    assert!(
        !message.contains("flaky: passed"),
        "a FAIL is never reported with the message for a pass: {result}"
    );
}

/// Without retries the same test simply fails, which is what makes the previous
/// test evidence that retries ran rather than that the test is not flaky.
#[test]
fn a_flaky_test_fails_without_retries() {
    let fixture = Fixture::new(PLAIN_CONFIG);
    let (result, code) = fixture.run_test("tests::flaky", &[]);

    assert_eq!(string(&result, "status"), "FAIL");
    assert_eq!(code, 100);
}

/// A target's environment reaches the test process, which is what lets a Buck2
/// target describe what its test needs rather than setting it on the runner.
#[test]
fn the_targets_environment_reaches_the_test() {
    let fixture = Fixture::new(PLAIN_CONFIG);
    let (result, code) = fixture.run_test(
        "tests::reads_target_env",
        &["--env", "BUCK2_NEXTEST_TARGET_ENV=from=the=target"],
    );

    assert_eq!(string(&result, "status"), "PASS", "{result}");
    assert_eq!(code, 0);
}

/// Without the flag the same test fails, which is what makes the previous test
/// evidence that `--env` carried the value rather than something else having
/// set it.
#[test]
fn a_test_needing_target_environment_fails_without_it() {
    let fixture = Fixture::new(PLAIN_CONFIG);
    let (result, code) = fixture.run_test("tests::reads_target_env", &[]);

    assert_eq!(string(&result, "status"), "FAIL");
    assert_eq!(code, 100);
}

/// A malformed `--env` is rejected rather than being dropped in silence, which
/// would leave the test running without something it was told it would have.
#[test]
fn a_malformed_env_pair_is_rejected() {
    let fixture = Fixture::new(PLAIN_CONFIG);
    let output = fixture.run(&[
        "run",
        "--env",
        "no-equals-sign",
        "--test-name",
        "tests::passes",
    ]);

    assert!(!output.status.success(), "a bare name is rejected");
    assert!(output.stdout.is_empty(), "nothing is reported to Buck2");

    let empty_name = fixture.run(&["run", "--env", "=value", "--test-name", "tests::passes"]);
    assert!(
        !empty_name.status.success(),
        "an empty variable name is rejected"
    );
}

/// The default-filter shapes the listing, but must not be applied again when
/// running: Buck2 chose this test from what it was told, and is waiting for a
/// result about it.
#[test]
fn a_test_outside_the_default_filter_still_runs_when_asked_for() {
    let fixture = Fixture::new(
        "[profile.narrowed]\ndefault-filter = 'test(=tests::passes)'\n\
         [profile.default]\n",
    );

    let listed = fixture.list(&["-P", "narrowed"]);
    let names: Vec<String> = listed.iter().map(filter_name).collect();
    assert_eq!(names, vec!["tests::passes"], "the listing is narrowed");

    let (result, code) = fixture.run_test("tests::fails", &["-P", "narrowed"]);
    assert_eq!(string(&result, "status"), "FAIL");
    assert_eq!(code, 100);
}

/// The project root is what nextest reads configuration from, and it is found
/// by walking up from the directory the action ran in.
#[test]
fn an_explicit_project_root_overrides_the_walk() {
    let fixture = Fixture::new("[profile.chosen]\nretries = 0\n[profile.default]\n");
    let elsewhere = Utf8TempDir::new().expect("a temporary directory");

    let output = Command::new(env!("CARGO_BIN_EXE_buck2-nextest"))
        .current_dir(elsewhere.path())
        .args(["list", "--label", LABEL])
        .args(["--program", fixture.program.as_str()])
        .args(["--project-root", fixture.root.as_str()])
        .args(["-P", "chosen"])
        .env_remove("NEXTEST_PROFILE")
        .output()
        .expect("buck2-nextest runs");

    assert!(
        output.status.success(),
        "listing succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(parse(&output).len(), 5);
}

/// A relative `--program` resolves against the project root rather than the
/// current directory, since Buck2 names artifacts relative to the project.
#[test]
fn a_relative_program_resolves_against_the_project_root() {
    let fixture = Fixture::new(PLAIN_CONFIG);
    let relative = fixture
        .program
        .strip_prefix(&fixture.root)
        .expect("the program is inside the project root");

    let output = Command::new(env!("CARGO_BIN_EXE_buck2-nextest"))
        .current_dir(&fixture.root)
        .args(["list", "--label", LABEL])
        .arg("--program")
        .arg(Utf8Path::new(relative))
        .env_remove("NEXTEST_PROFILE")
        .output()
        .expect("buck2-nextest runs");

    assert!(
        output.status.success(),
        "listing succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(parse(&output).len(), 5);
}

/// An ignored configuration key reaches the user rather than being dropped.
///
/// The warning is a `tracing` event, so it arrives only if the binary installs
/// a subscriber; without one the key is dropped in silence and the run quietly
/// uses a setting the user did not ask for.
#[test]
fn an_unknown_configuration_key_is_reported() {
    let fixture = Fixture::new("[profile.default]\nnot-a-real-key = true\n");
    let output = fixture.run(&["list"]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "listing succeeds:\n{stderr}");
    assert!(
        stderr.contains("ignoring unknown configuration") && stderr.contains("not-a-real-key"),
        "the ignored key is named on stderr:\n{stderr}"
    );
}
