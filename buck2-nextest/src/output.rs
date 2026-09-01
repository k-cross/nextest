// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The JSON Buck2's Starlark callbacks read.
//!
//! Buck2's callbacks are pure Starlark: they cannot run anything, so all the
//! judgement has to be here and the callback reduced to `json.decode`. That
//! puts the whole listing and result vocabulary in this module.
//!
//! Two shapes, matching what `InternalRunnerTestInfo` documents:
//!
//! * `parse_test_listing` reads a list of `{"name", "filter"}`, where `name` is
//!   displayed and `filter` is what selects the test to run.
//! * `parse_test_result` reads a list of `{"name", "status", "message",
//!   "duration", "details"}`.
//!
//! Buck2 appends a listing entry's `filter` as the final argument of the run
//! command, so the two are one contract: a `filter` here must be something
//! the run mode can select, and the `name` reported for a result must be
//! the `name` the listing gave, or Buck2 cannot match them up.

use nextest_session::{
    LiveSpec, MismatchReason, TestCaseName, TestInstanceId,
    events::{
        ChildExecutionOutputDescription, ChildOutputDescription, ExecuteStatus,
        ExecutionDescription, ExecutionResultDescription, ExecutionStatuses,
    },
};
use serde::Serialize;
use swrite::{SWrite, swrite};

/// One entry in the listing Buck2 reads.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ListedTest {
    /// What Buck2 displays for this test.
    pub(crate) name: String,

    /// What selects this test for execution.
    pub(crate) filter: String,
}

/// A test's outcome, in Buck2's vocabulary.
///
/// `OMITTED` is deliberately absent: it means a test Buck2 chose not to run,
/// which is a decision made before this binary is invoked at all.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum BuckTestStatus {
    /// The test passed.
    Pass,

    /// The test failed.
    Fail,

    /// The test was not run.
    Skip,

    /// The test was terminated for running too long.
    Timeout,
}

/// One result in the report Buck2 reads.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct TestResult {
    /// The name this test was listed under.
    pub(crate) name: String,

    /// How it went.
    pub(crate) status: BuckTestStatus,

    /// A one-line summary, shown against the result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) message: Option<String>,

    /// How long the test took, in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) duration: Option<f64>,

    /// The full diagnostic output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) details: Option<String>,
}

/// Returns the name a test is listed and reported under.
///
/// The target label is included because Buck2 shows results from every target
/// together, and the same test path can appear under more than one of them --
/// a per-module target and an all-in-one target compiling the same sources,
/// say. The bare test path stays the `filter`, since that is what the binary
/// itself understands.
pub(crate) fn display_name(label: &str, test_name: &TestCaseName) -> String {
    format!("{label} - {test_name}")
}

/// Builds the result for a test that ran to completion.
pub(crate) fn result_from_finished(
    label: &str,
    id: TestInstanceId<'_>,
    run_statuses: &ExecutionStatuses<LiveSpec>,
) -> TestResult {
    let name = display_name(label, id.test_name);

    let duration = run_statuses
        .iter()
        .map(|status| status.time_taken)
        .sum::<std::time::Duration>()
        .as_secs_f64();

    let (status, message, details) = match run_statuses.describe() {
        ExecutionDescription::Success { single_status } => {
            (BuckTestStatus::Pass, tolerated_message(single_status), None)
        }
        ExecutionDescription::Flaky {
            last_status,
            prior_statuses,
            result,
        } => {
            let attempts = last_status.retry_data.total_attempts;
            let attempt = last_status.retry_data.attempt;
            let message = format!(
                "flaky: passed on attempt {attempt} of {attempts}, after {} failed",
                prior_statuses.len()
            );
            let status = match result {
                nextest_session::FlakyResult::Pass => BuckTestStatus::Pass,
                nextest_session::FlakyResult::Fail => BuckTestStatus::Fail,
            };
            let details = prior_statuses.last().and_then(details_for);
            (status, Some(message), details)
        }
        ExecutionDescription::Failure { last_status, .. } => (
            failure_status(last_status),
            failure_message(last_status),
            details_for(last_status),
        ),
    };

    TestResult {
        name,
        status,
        message,
        duration: Some(duration),
        details,
    }
}

/// Builds the result for a test the filter did not select.
///
/// Buck2 listed this test and then asked for it, so it reached the run only to
/// be set aside -- almost always because it is `#[ignore]`d and the profile
/// does not ask for ignored tests.
pub(crate) fn result_from_skipped(
    label: &str,
    id: TestInstanceId<'_>,
    reason: MismatchReason,
) -> TestResult {
    TestResult {
        name: display_name(label, id.test_name),
        status: BuckTestStatus::Skip,
        message: Some(reason.to_string()),
        duration: None,
        details: None,
    }
}

/// Returns the status for an attempt that failed.
///
/// A nextest timeout is reported as such rather than folded into a plain
/// failure, since Buck2 has a status for it and the distinction is what tells
/// a slow test from a broken one. Buck2's own action timeout never reaches
/// here: it kills the action, and Buck2 decides the status without asking.
fn failure_status(status: &ExecuteStatus<LiveSpec>) -> BuckTestStatus {
    match &status.result {
        ExecutionResultDescription::Timeout { .. } => BuckTestStatus::Timeout,
        ExecutionResultDescription::Pass
        | ExecutionResultDescription::Leak { .. }
        | ExecutionResultDescription::Fail { .. }
        | ExecutionResultDescription::ExecFail => BuckTestStatus::Fail,
        _ => BuckTestStatus::Fail,
    }
}

/// Returns a note for an attempt that passed despite something notable.
///
/// A leak or a timeout the profile treats as a pass still counts as a pass, but
/// saying nothing at all would hide it: the run is green and the reason is only
/// in the configuration.
fn tolerated_message(status: &ExecuteStatus<LiveSpec>) -> Option<String> {
    match &status.result {
        ExecutionResultDescription::Leak { .. } => {
            Some("test passed but leaked handles within the leak timeout".to_owned())
        }
        ExecutionResultDescription::Timeout { .. } => {
            Some("test timed out, which this profile treats as a pass".to_owned())
        }
        ExecutionResultDescription::Pass
        | ExecutionResultDescription::Fail { .. }
        | ExecutionResultDescription::ExecFail => None,
        _ => None,
    }
}

/// Returns the one-line summary for an attempt that failed.
///
/// Nextest has usually already worked out the interesting line -- a panic
/// message, or the head of an error chain -- so prefer what it computed over
/// anything reconstructed here.
fn failure_message(status: &ExecuteStatus<LiveSpec>) -> Option<String> {
    let mut message = match (&status.error_summary, &status.output_error_slice) {
        (Some(summary), _) => summary.short_message.clone(),
        (None, Some(slice)) => slice.slice.clone(),
        (None, None) => match &status.result {
            ExecutionResultDescription::Fail { .. } => "test failed".to_owned(),
            ExecutionResultDescription::ExecFail => "the test process failed to start".to_owned(),
            ExecutionResultDescription::Timeout { .. } => {
                "test exceeded the slow timeout and was terminated".to_owned()
            }
            ExecutionResultDescription::Leak { .. } => {
                "test leaked handles past the leak timeout".to_owned()
            }
            ExecutionResultDescription::Pass => return None,
            _ => "test failed".to_owned(),
        },
    };

    if matches!(
        &status.result,
        ExecutionResultDescription::Fail { leaked: true, .. }
    ) {
        message.push_str("; the test also leaked handles");
    }

    Some(message)
}

/// Renders the diagnostic detail for an attempt.
///
/// Buck2 stores this against the result, so it is only produced for attempts
/// worth looking at -- a passing test's output would be stored for every test
/// in the repository and read for none of them.
fn details_for(status: &ExecuteStatus<LiveSpec>) -> Option<String> {
    let mut buf = String::new();

    if let Some(summary) = &status.error_summary {
        swrite!(buf, "{}\n", summary.description);
    }

    match &status.output {
        ChildExecutionOutputDescription::Output { output, .. } => match output {
            ChildOutputDescription::Split { stdout, stderr } => {
                append_section(
                    &mut buf,
                    "stdout",
                    stdout.as_ref().map(|out| &out.buf()[..]),
                );
                append_section(
                    &mut buf,
                    "stderr",
                    stderr.as_ref().map(|out| &out.buf()[..]),
                );
            }
            ChildOutputDescription::Combined { output } => {
                append_section(&mut buf, "output", Some(&output.buf()[..]));
            }
            ChildOutputDescription::NotLoaded => {}
        },
        ChildExecutionOutputDescription::StartError(error) => {
            swrite!(buf, "{error}\n");
        }
    }

    (!buf.is_empty()).then_some(buf)
}

/// Appends one captured stream, if it has anything in it.
///
/// Test output is whatever the test wrote, so it is not necessarily UTF-8;
/// Buck2's callback takes a string, so it is transcoded lossily rather than
/// dropped.
fn append_section(buf: &mut String, name: &str, bytes: Option<&[u8]>) {
    let Some(bytes) = bytes else { return };
    if bytes.is_empty() {
        return;
    }
    swrite!(buf, "--- {name} ---\n{}", String::from_utf8_lossy(bytes));
    if !bytes.ends_with(b"\n") {
        buf.push('\n');
    }
}
