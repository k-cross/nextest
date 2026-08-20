// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Listing and running tests described by a Buck2 spec.
//!
//! The pipeline itself -- profiles, filtersets, listing, running, reporting,
//! and the exit-code policy -- is `nextest-session`'s. This module supplies
//! Buck2's inputs and policy: the converted binary list, where output goes,
//! and how a run with no tests in it is judged.

use crate::{
    convert::Buck2BinaryList,
    errors::{ExpectedError, Result},
};
use camino::Utf8PathBuf;
use nextest_session::{
    ConfigExperimental, EarlyProfile, EnvironmentMap, EvaluatableProfile, FilterBound,
    FiltersetKind, InputHandlerKind, KnownGroups, ListProgressOptions, NextestConfig,
    NextestRunMode, NoTestsBehavior, OutputFormat, ParseContext, PathMapper, ReportUuid, Reporter,
    ReporterBuilder, ReporterEvent, ReporterOutput, RunIgnored, SessionContext, SessionInputs,
    ShowProgress, ShowTerminalProgress, SignalHandlerKind, StructuredReporter, TestFilter,
    TestFilterPatterns, TestListOptions, TestRunnerBuilder, TestSession, ThemeCharacters, WriteStr,
    errors::{ExecuteError, SessionBuildError},
    evaluate_profile, final_outcome, parse_filtersets, run_to_completion,
};
use std::{
    convert::Infallible,
    fmt,
    io::{IsTerminal, Write},
    sync::Arc,
};

/// Where the displayer's output goes, and so who owns the terminal.
///
/// A plain flag rather than a [`ReporterOutput`], which borrows a writer and is
/// invariant over its lifetime, so it has to be built where it is used.
#[derive(Clone, Copy, Debug)]
enum OutputTo {
    /// The terminal, as `cargo-nextest` does.
    Terminal,

    /// Standard error, plainly, for when something else owns the terminal.
    ///
    /// Buck2 captures the executor's standard error and shows it only on
    /// request (`buck2 test --test-executor-stderr=-`), so this is a detail
    /// view rather than the primary one -- which is why it is written without
    /// a progress bar or any other cursor control.
    PlainStderr,
}

impl OutputTo {
    /// Returns how to handle signals and terminal input.
    ///
    /// Input handling is interactive: it reads standard input to offer nextest's
    /// info and pause features. That only makes sense when nextest is what the
    /// person is looking at, and under Buck2 standard input belongs to Buck2.
    ///
    /// Signals are the other way round. Whoever spawned the test processes has
    /// to shut them down, and that is nextest either way -- so `Ctrl-C` must
    /// still reach the graceful cancellation path rather than killing this
    /// process and orphaning the tests it started.
    fn handlers(self) -> (SignalHandlerKind, InputHandlerKind) {
        match self {
            Self::Terminal => (SignalHandlerKind::Standard, InputHandlerKind::Standard),
            Self::PlainStderr => (SignalHandlerKind::Standard, InputHandlerKind::Noop),
        }
    }
}

/// Writes the displayer's output to standard error, without any cursor control.
struct PlainStderrWriter;

impl WriteStr for PlainStderrWriter {
    fn write_str(&mut self, s: &str) -> std::io::Result<()> {
        std::io::stderr().write_all(s.as_bytes())
    }

    fn write_str_flush(&mut self) -> std::io::Result<()> {
        std::io::stderr().flush()
    }
}

/// Everything needed to list or run the tests in a spec.
pub struct RunContext {
    /// The converted binary list and its packages.
    pub binaries: Buck2BinaryList,

    /// The Buck2 project root.
    pub project_root: Utf8PathBuf,

    /// The nextest profile to use.
    pub profile_name: Option<String>,

    /// A path to a nextest configuration file, if one was given.
    pub config_file: Option<Utf8PathBuf>,

    /// The ID that identifies this run.
    ///
    /// One ID covers both listing and execution, so that a test binary sees the
    /// same `NEXTEST_RUN_ID` in both phases and can key state on it across them.
    pub run_id: ReportUuid,

    /// Filtersets from the command line.
    pub filtersets: Vec<String>,

    /// Substring filters from the command line.
    pub filter_patterns: Vec<String>,

    /// Whether to run ignored tests.
    pub run_ignored: RunIgnored,

    /// What the filtersets are bounded by.
    pub filter_bound: FilterBound,

    /// The number of threads to list tests with.
    pub list_threads: usize,

    /// How to treat a run in which no tests were selected.
    ///
    /// `None` is nextest's default: such a run is an error.
    pub no_tests: Option<NoTestsBehavior>,
}

impl RunContext {
    /// Lists the tests in the spec, writing them in the requested format.
    pub fn list(&self, format: OutputFormat, writer: &mut dyn WriteStr) -> Result<()> {
        let config = self.load_config()?;
        let early_profile = self.load_profile(&config)?;
        let filter = self.build_filter(&early_profile.known_groups())?;
        let profile = self.evaluate_profile(early_profile)?;
        let ctx = self.session_context();
        let session = self.build_session(&ctx, &profile, &filter)?;

        session
            .test_list()
            .write(format, writer, false)
            .map_err(|error| ExpectedError::WriteEventError {
                error: std::io::Error::other(error),
            })
    }

    /// Runs the tests in the spec, returning the process exit code.
    pub fn run(&self, cli_args: Vec<String>) -> Result<i32> {
        self.run_inner(cli_args, OutputTo::Terminal, |_| Ok::<(), Infallible>(()))
    }

    /// Runs the tests, forwarding every event to `sink` as well.
    ///
    /// Used when something other than the terminal consumes results -- Buck2,
    /// which renders them itself. The displayer still runs, writing plainly to
    /// standard error, so its output is there for anyone who asks Buck2 for it.
    ///
    /// If `sink` returns an error, the run is cancelled gracefully: nextest
    /// keeps reporting until the tests it has already started finish.
    pub fn run_with_sink<E, F>(&self, cli_args: Vec<String>, sink: F) -> Result<i32>
    where
        F: FnMut(&ReporterEvent<'_>) -> std::result::Result<(), E> + Send,
        E: fmt::Debug + Send,
    {
        self.run_inner(cli_args, OutputTo::PlainStderr, sink)
    }

    fn run_inner<E, F>(&self, cli_args: Vec<String>, output: OutputTo, sink: F) -> Result<i32>
    where
        F: FnMut(&ReporterEvent<'_>) -> std::result::Result<(), E> + Send,
        E: fmt::Debug + Send,
    {
        let config = self.load_config()?;
        let early_profile = self.load_profile(&config)?;
        let filter = self.build_filter(&early_profile.known_groups())?;
        let profile = self.evaluate_profile(early_profile)?;
        let ctx = self.session_context();
        let session = self.build_session(&ctx, &profile, &filter)?;

        let (signal_handler, input_handler) = output.handlers();
        let runner = session
            .build_runner(
                TestRunnerBuilder::default(),
                cli_args,
                signal_handler,
                input_handler,
            )
            .map_err(|error| ExpectedError::TestRunnerBuildError { error })?;

        // The writer must be declared here, next to the borrows it shares a
        // lifetime with: `ReporterOutput` is invariant, so one built further
        // out cannot be narrowed to this scope.
        let mut plain_stderr = PlainStderrWriter;
        let output = match output {
            OutputTo::Terminal => ReporterOutput::Terminal,
            OutputTo::PlainStderr => ReporterOutput::Writer {
                writer: &mut plain_stderr,
                // Whatever is downstream of Buck2's capture is unknown, so
                // stick to ASCII.
                use_unicode: false,
            },
        };

        let reporter: Reporter<'_> = ReporterBuilder::default().build(
            session.test_list(),
            &profile,
            ShowTerminalProgress::No,
            output,
            StructuredReporter::new(),
        );

        let executed = run_to_completion(runner, reporter, false, sink).map_err(
            |error: ExecuteError<E>| ExpectedError::WriteEventError {
                error: std::io::Error::other(error.to_string()),
            },
        )?;

        match final_outcome(
            NextestRunMode::Test,
            executed.run_stats,
            self.no_tests,
            None,
            false,
        ) {
            Ok(()) => Ok(0),
            Err(failure) => Ok(failure.exit_code()),
        }
    }

    // ---
    // Helper methods
    // ---

    fn load_config(&self) -> Result<NextestConfig> {
        // Buck2 has no Cargo package graph, so package-graph filterset
        // predicates are unavailable. See `ParseContext::without_graph`.
        let pcx = ParseContext::without_graph();

        NextestConfig::from_sources(
            self.project_root.clone(),
            &pcx,
            self.config_file.as_deref(),
            &[],
            &ConfigExperimental::from_env(),
        )
        .map_err(|error| ExpectedError::ConfigParseError { error })
    }

    fn load_profile<'cfg>(&self, config: &'cfg NextestConfig) -> Result<EarlyProfile<'cfg>> {
        let name = self
            .profile_name
            .as_deref()
            .unwrap_or(NextestConfig::DEFAULT_PROFILE);
        config
            .profile(name)
            .map_err(|error| ExpectedError::ProfileNotFound { error })
    }

    fn evaluate_profile<'cfg>(
        &self,
        early_profile: EarlyProfile<'cfg>,
    ) -> Result<EvaluatableProfile<'cfg>> {
        evaluate_profile(
            early_profile,
            &self.binaries.binary_list.rust_build_meta.build_platforms,
        )
        .map_err(|error| ExpectedError::StoreDirCreateError { error })
    }

    /// Builds the test filter.
    ///
    /// `known_groups` comes from the profile: `group()` is legal in a test
    /// filterset, so the set of valid group names must be known before the
    /// filterset is compiled.
    fn build_filter(&self, known_groups: &KnownGroups) -> Result<TestFilter> {
        let pcx = ParseContext::without_graph();
        let exprs = parse_filtersets(&pcx, &self.filtersets, FiltersetKind::Test, known_groups)
            .map_err(|all_errors| ExpectedError::FiltersetParseError { all_errors })?;

        TestFilter::new(
            NextestRunMode::Test,
            self.run_ignored,
            TestFilterPatterns::new(self.filter_patterns.clone()),
            exprs,
        )
        .map_err(|error| ExpectedError::TestFilterBuildError { error })
    }

    fn session_context(&self) -> SessionContext {
        SessionContext::simple(
            self.run_id,
            semver::Version::parse(env!("CARGO_PKG_VERSION"))
                .expect("crate version is valid semver"),
        )
    }

    fn build_session<'a>(
        &'a self,
        ctx: &'a SessionContext,
        profile: &'a EvaluatableProfile<'a>,
        filter: &TestFilter,
    ) -> Result<TestSession<'a>> {
        TestSession::build(
            ctx,
            profile,
            SessionInputs {
                binary_list: Arc::new(self.binaries.binary_list.clone()),
                packages: &self.binaries.packages,
                workspace_root: self.project_root.clone(),
                env: EnvironmentMap::empty(),
                // No path remapping: the spec's paths are already where the
                // binaries are.
                path_mapper: PathMapper::noop(),
            },
            filter,
            TestListOptions {
                partitioner_builder: None,
                platform_filter: None,
                filter_bound: self.filter_bound,
                list_threads: self.list_threads,
                progress: ListProgressOptions::new(
                    ShowProgress::default(),
                    ShowTerminalProgress::No,
                    ThemeCharacters::default(),
                    std::io::stderr().is_terminal(),
                ),
            },
        )
        .map_err(|error| match error {
            SessionBuildError::FromMessages(error) => ExpectedError::FromMessagesError { error },
            SessionBuildError::CreateTestList(error) => {
                ExpectedError::CreateTestListError { error }
            }
        })
    }
}
