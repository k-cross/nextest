// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The session itself: a test list, and running it to completion.

use crate::{
    context::SessionContext,
    errors::{ExecuteError, SessionBuildError, SinkError},
    input::{SessionInputs, TestListOptions},
};
use nextest_runner::{
    config::core::EvaluatableProfile,
    errors::TestRunnerBuildError,
    input::InputHandlerKind,
    list::{RustTestArtifact, TestList},
    reporter::{
        Reporter, ReporterStats,
        events::{ReporterEvent, RunStats},
    },
    runner::{TestRunner, TestRunnerBuilder, configure_handle_inheritance},
    signal::SignalHandlerKind,
    test_filter::TestFilter,
};
use std::fmt;

/// A test list built from a build system's inputs, ready to write out or run.
///
/// This owns the middle of the pipeline: building it enumerates the tests, and
/// [`build_runner`](Self::build_runner) turns the result into a runner.
/// Reporter construction stays with the frontend, because a
/// [`ReporterOutput`](nextest_runner::reporter::ReporterOutput) borrows its
/// writer invariantly and so must be built next to it.
pub struct TestSession<'a> {
    ctx: &'a SessionContext,
    profile: &'a EvaluatableProfile<'a>,
    test_list: TestList<'a>,
}

impl<'a> TestSession<'a> {
    /// Builds the test list from the session's inputs.
    ///
    /// This is the phase that executes each test binary to enumerate its
    /// tests.
    pub fn build(
        ctx: &'a SessionContext,
        profile: &'a EvaluatableProfile<'a>,
        inputs: SessionInputs<'a>,
        test_filter: &TestFilter,
        options: TestListOptions<'_>,
    ) -> Result<Self, SessionBuildError> {
        let SessionInputs {
            binary_list,
            packages,
            workspace_root,
            env,
            path_mapper,
        } = inputs;

        // Use the canonicalized workspace root from the path mapper if a remap
        // was specified, so `NEXTEST_WORKSPACE_ROOT` is an absolute, normalized
        // path consistent with `CARGO_MANIFEST_DIR`.
        let workspace_root = match path_mapper.new_workspace_root() {
            Some(canonical) => canonical.to_owned(),
            None => workspace_root,
        };

        let rust_build_meta = binary_list.rust_build_meta.map_paths(&path_mapper);
        let artifacts = RustTestArtifact::from_binary_list(
            packages,
            binary_list,
            &rust_build_meta,
            &path_mapper,
            options.platform_filter,
        )?;

        let test_list = TestList::new(
            &ctx.test_execute_context(profile.name()),
            artifacts,
            rust_build_meta,
            test_filter,
            options.partitioner_builder,
            workspace_root,
            env,
            profile,
            options.filter_bound,
            options.list_threads,
            options.progress,
        )?;

        Ok(Self {
            ctx,
            profile,
            test_list,
        })
    }

    /// Returns the test list.
    pub fn test_list(&self) -> &TestList<'a> {
        &self.test_list
    }

    /// Builds a runner for the session's tests.
    pub fn build_runner(
        &'a self,
        runner_builder: TestRunnerBuilder,
        cli_args: Vec<String>,
        signal_handler: SignalHandlerKind,
        input_handler: InputHandlerKind,
    ) -> Result<TestRunner<'a>, TestRunnerBuildError> {
        runner_builder.build(
            self.ctx.run_id,
            self.ctx.version_env_vars.clone(),
            &self.test_list,
            self.profile,
            cli_args,
            signal_handler,
            input_handler,
            self.ctx.double_spawn.clone(),
            self.ctx.target_runner.clone(),
        )
    }
}

/// What a completed run produced.
#[derive(Debug)]
pub struct ExecutedRun {
    /// Statistics for the run.
    pub run_stats: RunStats,

    /// What the reporter accumulated along the way.
    pub reporter_stats: ReporterStats,
}

/// Runs the tests to completion, feeding every event through `sink` and then
/// the reporter.
///
/// The sink goes first: if delivery has failed there is no point rendering an
/// event nobody will see, and returning its error is what starts a graceful
/// cancellation -- nextest keeps reporting until the tests it has already
/// started finish. A frontend with no sink passes `|_| Ok::<(), Infallible>(())`
/// and recovers the reporter's own error type with
/// [`into_report_errors`](crate::into_report_errors).
///
/// Consumes the reporter and calls its `finish`, so a frontend cannot forget
/// to.
pub fn run_to_completion<'a, E, F>(
    runner: TestRunner<'a>,
    mut reporter: Reporter<'a>,
    no_capture: bool,
    mut sink: F,
) -> Result<ExecutedRun, ExecuteError<E>>
where
    F: FnMut(&ReporterEvent<'a>) -> Result<(), E> + Send,
    E: fmt::Debug + Send,
{
    configure_handle_inheritance(no_capture).map_err(ExecuteError::ConfigureHandleInheritance)?;
    let run_stats = runner
        .try_execute(|event| {
            sink(&event).map_err(SinkError::Sink)?;
            reporter.report_event(event).map_err(SinkError::Report)
        })
        .map_err(ExecuteError::Execute)?;
    let reporter_stats = reporter.finish();
    Ok(ExecutedRun {
        run_stats,
        reporter_stats,
    })
}
