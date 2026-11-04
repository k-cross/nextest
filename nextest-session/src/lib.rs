// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

#![warn(missing_docs)]

//! The build-system integration pipeline for nextest.
//!
//! Nextest's runner does not care who built the test binaries. This crate is
//! the contract between a build system and nextest: a frontend supplies what
//! it built, and this crate carries it through nextest's shared pipeline --
//! configuration, profiles, filtersets, listing, running, and reporting.
//! `cargo-nextest` (Cargo) and `buck2-nextest` (Buck2) are both frontends of
//! this crate; a new build system integration starts here.
//!
//! # What a build system supplies
//!
//! The inputs are data, not callbacks:
//!
//! * [`RustTestBinary`], one per test binary, collected into a [`BinaryList`]
//!   along with a [`RustBuildMeta`] describing the build.
//! * [`PackageInfo`], one per package named by a binary's `package_id`. A
//!   build system without Cargo's package vocabulary synthesizes these; see
//!   how `buck2-nextest` derives them from Buck2 labels. Its `manifest_dir`
//!   is the directory the package's tests run in.
//! * [`TestBinaryInvocation`], per binary, for launchers that need extra
//!   leading arguments or environment variables.
//! * A workspace root, and [`BuildPlatforms`] for the host and target.
//!
//! # What the pipeline provides
//!
//! In order:
//!
//! 1. [`NextestConfig::from_sources`] loads configuration from
//!    `.config/nextest.toml` under the workspace root, with a [`ParseContext`]
//!    -- [`ParseContext::without_graph`] when there is no Cargo package graph,
//!    which disables the package-graph filterset predicates and nothing else.
//! 2. [`EarlyProfile::apply_build_platforms`] turns the profile into an
//!    [`EvaluatableProfile`].
//! 3. [`parse_filtersets`] compiles filterset inputs against the profile's
//!    known test groups, reporting every bad one at once.
//! 4. [`TestSession::build`] executes the binaries to enumerate their tests,
//!    producing a [`TestList`] to write out (for listing) or run. A build
//!    system that already knows each binary's tests can use
//!    [`TestSession::build_with_known_tests`], which skips executing them.
//! 5. [`TestSession::build_runner`] and [`run_to_completion`] execute the
//!    tests, showing every event to the frontend's observer and reporter.
//! 6. [`final_outcome`] maps the finished run to the canonical exit-code
//!    policy, shared so frontends cannot drift apart.
//!
//! Reporter construction stays with the frontend: a
//! [`ReporterOutput`] borrows its writer invariantly, so it must be built in
//! the scope that owns the writer. Frontend policy in general -- CLI parsing,
//! user configuration, pagers, recording -- stays out of this crate.
//!
//! # Example
//!
//! A minimal integration: one prebuilt test binary, run with the default
//! profile.
//!
//! ```no_run
//! use camino::Utf8PathBuf;
//! use iddqd::IdOrdMap;
//! use nextest_session::{
//!     BinaryList, BuildPlatform, BuildPlatforms, ConfigExperimental, EnvironmentMap, FilterBound,
//!     InputHandlerKind, ListProgressOptions, NextestConfig, NextestRunMode, NoTestsBehavior,
//!     PackageId, PackageInfo, ParseContext, PathMapper, ReporterBuilder, ReporterOutput,
//!     RunIgnored, RustBinaryId, RustBuildMeta, RustTestBinary, RustTestBinaryKind,
//!     SessionContext, SessionInputs, ShowProgress, ShowTerminalProgress, SignalHandlerKind,
//!     StructuredReporter, TestBinaryInvocation, TestFilter, TestFilterPatterns, TestListOptions,
//!     TestRunnerBuilder, TestSession, ThemeCharacters, final_outcome, force_or_new_run_id,
//!     run_to_completion,
//! };
//! use semver::Version;
//! use std::sync::Arc;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let project_root = Utf8PathBuf::from("/path/to/project");
//!
//! // Describe what the build system built. Every package a binary names must
//! // have an entry in `packages`.
//! let package_id = "my-build//:my-test".to_owned();
//! let mut packages = IdOrdMap::new();
//! packages.insert_overwrite(PackageInfo::new(
//!     PackageId::new(package_id.clone()),
//!     "my-test".to_owned(),
//!     Version::new(0, 0, 0),
//!     project_root.clone(),
//! ));
//! let build_platforms = BuildPlatforms::new_with_no_target()?;
//! let binary_list = Arc::new(BinaryList {
//!     rust_build_meta: RustBuildMeta::new(&project_root, &project_root, build_platforms),
//!     rust_binaries: vec![RustTestBinary {
//!         id: RustBinaryId::new("my-test"),
//!         path: project_root.join("out/my-test"),
//!         package_id,
//!         kind: RustTestBinaryKind::TEST,
//!         name: "my-test".to_owned(),
//!         build_platform: BuildPlatform::Target,
//!         invocation: TestBinaryInvocation::empty(),
//!     }],
//! });
//!
//! // Load configuration and evaluate the profile.
//! let pcx = ParseContext::without_graph();
//! let config = NextestConfig::from_sources(
//!     project_root.clone(),
//!     &pcx,
//!     None,
//!     &[],
//!     &ConfigExperimental::from_env(),
//! )?;
//! let profile = config
//!     .profile(NextestConfig::DEFAULT_PROFILE)?
//!     .apply_build_platforms(&binary_list.rust_build_meta.build_platforms);
//!
//! // Build the test list.
//! let filter = TestFilter::new(
//!     NextestRunMode::Test,
//!     RunIgnored::Default,
//!     TestFilterPatterns::new(Vec::new()),
//!     Vec::new(),
//! )?;
//! let ctx = SessionContext::simple(force_or_new_run_id(), Version::new(0, 1, 0));
//! let session = TestSession::build(
//!     &ctx,
//!     &profile,
//!     SessionInputs {
//!         binary_list,
//!         packages: &packages,
//!         workspace_root: project_root,
//!         env: EnvironmentMap::empty(),
//!         path_mapper: PathMapper::noop(),
//!     },
//!     &filter,
//!     TestListOptions {
//!         partitioner_builder: None,
//!         platform_filter: None,
//!         filter_bound: FilterBound::DefaultSet,
//!         list_threads: 1,
//!         progress: ListProgressOptions::new(
//!             ShowProgress::default(),
//!             ShowTerminalProgress::No,
//!             ThemeCharacters::default(),
//!             false,
//!         ),
//!     },
//! )?;
//!
//! // Run the tests and map the outcome to an exit code.
//! let runner = session.build_runner(
//!     TestRunnerBuilder::default(),
//!     std::env::args().collect(),
//!     SignalHandlerKind::Standard,
//!     InputHandlerKind::Noop,
//! )?;
//! let reporter = ReporterBuilder::default().build(
//!     session.test_list(),
//!     &profile,
//!     ShowTerminalProgress::No,
//!     ReporterOutput::Terminal,
//!     StructuredReporter::new(),
//! );
//! let executed = run_to_completion(runner, reporter, false, |_event| {})?;
//! match final_outcome(
//!     NextestRunMode::Test,
//!     executed.outcome,
//!     Some(NoTestsBehavior::Fail),
//!     None,
//! ) {
//!     Ok(()) => Ok(()),
//!     Err(failure) => std::process::exit(failure.exit_code()),
//! }
//! # }
//! ```

mod context;
pub mod errors;
mod filter;
mod input;
mod outcome;
mod session;

pub use context::SessionContext;
pub use filter::parse_filtersets;
pub use guppy::PackageId;
pub use iddqd::IdOrdMap;
pub use input::{KnownTestListOptions, SessionInputs, TestListOptions};
pub use nextest_filtering::{Filterset, FiltersetKind, KnownGroups, ParseContext};
pub use nextest_metadata::{
    BuildPlatform, FilterMatch, MismatchReason, NextestExitCode, RustBinaryId, RustTestBinaryKind,
    RustTestKind, TestCaseName,
};
pub use nextest_runner::{
    cargo_config::EnvironmentMap,
    config::core::{ConfigExperimental, EarlyProfile, EvaluatableProfile, NextestConfig},
    double_spawn::DoubleSpawnInfo,
    helpers::{ShowTerminalProgress, ThemeCharacters, force_or_new_run_id, plural},
    input::InputHandlerKind,
    list::{
        BinaryList, BinaryListState, ListProgressOptions, OutputFormat, OwnedTestInstanceId,
        PackageInfo, RustBuildMeta, RustTestBinary, RustTestSuite, SerializableFormat,
        TestBinaryInvocation, TestExecuteContext, TestInstance, TestInstanceId, TestList,
        TestListState, UnfilteredTestCase,
    },
    output_spec::LiveSpec,
    partition::PartitionerBuilder,
    platform::BuildPlatforms,
    reporter::{
        Reporter, ReporterBuilder, ReporterOutput, ReporterStats, ShowProgress, events,
        events::{ReporterEvent, RunOutcome},
        structured::StructuredReporter,
    },
    reuse_build::PathMapper,
    run_mode::NextestRunMode,
    runner::{TestRunner, TestRunnerBuilder, VersionEnvVars},
    signal::SignalHandlerKind,
    target_runner::TargetRunner,
    test_filter::{FilterBound, RunIgnored, TestFilter, TestFilterPatterns},
    test_output::ChildSingleOutput,
    write_str::WriteStr,
};
pub use outcome::{NoTestsBehavior, RunFailure, final_outcome};
pub use quick_junit::ReportUuid;
pub use session::{ExecutedRun, TestSession, run_to_completion};
