// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
//!   how `buck2-nextest` derives them from Buck2 labels.
//! * [`TestBinaryInvocation`], per binary, for launchers that need extra
//!   leading arguments, environment variables, or a working directory.
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
//! 2. [`evaluate_profile`] turns an [`EarlyProfile`] into an
//!    [`EvaluatableProfile`], creating the profile's store directory if it
//!    writes a JUnit report.
//! 3. [`parse_filtersets`] compiles filterset inputs against the profile's
//!    known test groups, reporting every bad one at once.
//! 4. [`TestSession::build`] executes the binaries to enumerate their tests,
//!    producing a [`TestList`] to write out (for listing) or run.
//! 5. [`TestSession::build_runner`] and [`run_to_completion`] execute the
//!    tests, feeding every event to the frontend's reporter and optional sink.
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
//!     TestRunnerBuilder, TestSession, ThemeCharacters, evaluate_profile, final_outcome,
//!     force_or_new_run_id, run_to_completion,
//! };
//! use semver::Version;
//! use std::{convert::Infallible, sync::Arc};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let project_root = Utf8PathBuf::from("/path/to/project");
//!
//! // Describe what the build system built. Every package a binary names must
//! // have an entry in `packages`.
//! let package_id = "my-build//:my-test".to_owned();
//! let mut packages = IdOrdMap::new();
//! packages.insert_overwrite(PackageInfo {
//!     id: PackageId::new(package_id.clone()),
//!     name: "my-test".to_owned(),
//!     version: Version::new(0, 0, 0),
//!     authors: Vec::new(),
//!     description: None,
//!     homepage: None,
//!     license: None,
//!     license_file: None,
//!     repository: None,
//!     minimum_rust_version: None,
//!     manifest_path: project_root.join("BUILD"),
//! });
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
//! let profile = evaluate_profile(
//!     config.profile(NextestConfig::DEFAULT_PROFILE)?,
//!     &binary_list.rust_build_meta.build_platforms,
//! )?;
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
//! let executed = run_to_completion(runner, reporter, false, |_event| Ok::<_, Infallible>(()))?;
//! match final_outcome(
//!     NextestRunMode::Test,
//!     executed.run_stats,
//!     Some(NoTestsBehavior::Fail),
//!     None,
//!     false,
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
mod profile;
mod session;

pub use context::SessionContext;
pub use errors::into_report_errors;
pub use filter::parse_filtersets;
// Re-exports of everything an integration constructs or consumes, so the
// contract is one crate. Frontends that already depend on `nextest-runner`
// directly may keep importing from it; these names are the same types.

// The inputs an integration supplies.
pub use guppy::PackageId;
pub use iddqd::IdOrdMap;
pub use input::{SessionInputs, TestListOptions};
// Filtering.
pub use nextest_filtering::{Filterset, FiltersetKind, KnownGroups, ParseContext};
// Identity and metadata vocabulary.
pub use nextest_metadata::{BuildPlatform, NextestExitCode, RustBinaryId, RustTestBinaryKind};
// Configuration and profiles.
pub use nextest_runner::config::core::{
    ConfigExperimental, EarlyProfile, EvaluatableProfile, NextestConfig, get_num_cpus,
};
// Listing.
pub use nextest_runner::{
    cargo_config::EnvironmentMap,
    list::{ListProgressOptions, OutputFormat, SerializableFormat, TestExecuteContext, TestList},
};
// Running.
pub use nextest_runner::{
    double_spawn::DoubleSpawnInfo,
    helpers::force_or_new_run_id,
    input::InputHandlerKind,
    run_mode::NextestRunMode,
    runner::{TestRunner, TestRunnerBuilder, VersionEnvVars},
    signal::SignalHandlerKind,
    target_runner::TargetRunner,
};
// Reporting.
pub use nextest_runner::{
    helpers::{ShowTerminalProgress, ThemeCharacters},
    reporter::{
        Reporter, ReporterBuilder, ReporterOutput, ReporterStats, ShowProgress,
        events::{FinalRunStats, ReporterEvent, RunStats},
        structured::StructuredReporter,
    },
    write_str::WriteStr,
};
pub use nextest_runner::{
    list::{
        BinaryList, BinaryListState, PackageInfo, RustBuildMeta, RustTestBinary,
        TestBinaryInvocation, TestListState,
    },
    partition::PartitionerBuilder,
    platform::BuildPlatforms,
    reuse_build::PathMapper,
    test_filter::{FilterBound, RunIgnored, TestFilter, TestFilterPatterns},
};
pub use outcome::{NoTestsBehavior, RunFailure, final_outcome};
pub use profile::{create_junit_store_dir, evaluate_profile};
pub use quick_junit::ReportUuid;
pub use session::{ExecutedRun, TestSession, run_to_completion};
