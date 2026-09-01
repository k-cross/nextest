// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Running exactly one test from one Buck2 test target.
//!
//! This is the `command` Buck2 runs once per discovered test, with the test's
//! `filter` from the listing appended as the final argument. Buck2 schedules
//! and caches; everything within the test is nextest's, because this drives the
//! real pipeline: the profile's retries, slow-test handling, leak detection,
//! and the environment a test sees all apply exactly as they would under
//! `cargo nextest run`.

use crate::{
    errors::{ExpectedError, Result},
    output::{BuckTestStatus, TestResult, result_from_finished, result_from_skipped},
    pipeline::{Context, PlainStderrWriter},
};
use nextest_session::{
    FilterBound, InputHandlerKind, NextestExitCode, NextestRunMode, Reporter, ReporterBuilder,
    ReporterEvent, ReporterOutput, RunIgnored, ShowTerminalProgress, SignalHandlerKind,
    StructuredReporter, TestFilter, TestFilterPatterns, TestRunnerBuilder, WriteStr,
    errors::ExecuteError, events::TestEventKind, run_to_completion,
};
use std::convert::Infallible;

/// Runs one test, writing its result as the JSON Buck2's `parse_test_result`
/// reads, and returns the process exit code.
pub(crate) fn run_one(
    cx: &Context,
    test_name: &str,
    cli_args: Vec<String>,
    writer: &mut dyn WriteStr,
) -> Result<i32> {
    let config = cx.load_config()?;
    let early_profile = cx.load_profile(&config)?;
    let profile = cx.evaluate_profile(early_profile)?;
    let ctx = cx.session_context();

    let mut patterns = TestFilterPatterns::new(Vec::new());
    patterns.add_exact_pattern(test_name.to_owned());

    let filter = TestFilter::new(
        NextestRunMode::Test,
        RunIgnored::Default,
        patterns,
        Vec::new(),
    )
    .map_err(|error| ExpectedError::TestFilterBuildError { error })?;

    let session = cx.build_session(&ctx, &profile, &filter, FilterBound::All)?;

    let runner = session
        .build_runner(
            TestRunnerBuilder::default(),
            cli_args,
            SignalHandlerKind::Standard,
            InputHandlerKind::Noop,
        )
        .map_err(|error| ExpectedError::TestRunnerBuildError { error })?;

    let mut plain_stderr = PlainStderrWriter;
    let reporter: Reporter<'_> = ReporterBuilder::default().build(
        session.test_list(),
        &profile,
        ShowTerminalProgress::No,
        ReporterOutput::Writer {
            writer: &mut plain_stderr,
            use_unicode: false,
        },
        StructuredReporter::new(),
    );

    let mut result: Option<TestResult> = None;
    run_to_completion(runner, reporter, false, |event| {
        capture(&cx.label, test_name, event, &mut result);
        Ok::<(), Infallible>(())
    })
    .map_err(
        |error: ExecuteError<Infallible>| ExpectedError::WriteEventError {
            error: std::io::Error::other(error.to_string()),
        },
    )?;

    let Some(result) = result else {
        return Err(ExpectedError::TestNotFound {
            label: cx.label.clone(),
            test_name: test_name.to_owned(),
        });
    };

    let status = result.status;
    let json = serde_json::to_string(&[result])
        .map_err(|error| ExpectedError::ResultSerializeError { error })?;
    writer
        .write_str(&json)
        .and_then(|()| writer.write_str("\n"))
        .and_then(|()| writer.write_str_flush())
        .map_err(|error| ExpectedError::WriteEventError { error })?;

    Ok(match status {
        BuckTestStatus::Pass | BuckTestStatus::Skip => 0,
        BuckTestStatus::Fail | BuckTestStatus::Timeout => NextestExitCode::TEST_RUN_FAILED,
    })
}

/// Records the result for the one test this invocation is about.
///
/// The name is matched rather than taking whatever arrives, because the other
/// tests in the binary are reported too: everything the exact pattern did not
/// select is skipped, and each skip is an event. Taking the last one to arrive
/// would answer Buck2's question about one test with another test's result.
fn capture(
    label: &str,
    test_name: &str,
    event: &ReporterEvent<'_>,
    result: &mut Option<TestResult>,
) {
    let ReporterEvent::Test(event) = event else {
        return;
    };
    match &event.kind {
        TestEventKind::TestFinished {
            test_instance,
            run_statuses,
            ..
        } if test_instance.test_name.as_str() == test_name => {
            *result = Some(result_from_finished(label, *test_instance, run_statuses));
        }
        TestEventKind::TestSkipped {
            test_instance,
            reason,
            ..
        } if test_instance.test_name.as_str() == test_name => {
            *result = Some(result_from_skipped(label, *test_instance, *reason));
        }
        _ => {}
    }
}
