// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The session itself: a test list, and running it to completion.

use crate::{
    context::SessionContext,
    errors::{ExecuteError, KnownTestsBuildError, SessionBuildError, SinkError},
    input::{KnownTestListOptions, SessionInputs, TestListOptions},
};
use camino::Utf8PathBuf;
use iddqd::IdOrdMap;
use nextest_metadata::{BuildPlatform, RustBinaryId};
use nextest_runner::{
    cargo_config::EnvironmentMap,
    config::core::EvaluatableProfile,
    errors::{FromMessagesError, TestRunnerBuildError},
    input::InputHandlerKind,
    list::{RustBuildMeta, RustTestArtifact, TestList, TestListState, UnfilteredTestCase},
    reporter::{
        Reporter, ReporterStats,
        events::{ReporterEvent, RunStats},
    },
    runner::{TestRunner, TestRunnerBuilder, configure_handle_inheritance},
    signal::SignalHandlerKind,
    test_filter::TestFilter,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

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
        let prepared = PreparedInputs::new(inputs, options.platform_filter)?;

        let test_list = TestList::new(
            &ctx.test_execute_context(profile.name()),
            prepared.artifacts,
            prepared.rust_build_meta,
            test_filter,
            options.partitioner_builder,
            prepared.workspace_root,
            prepared.env,
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

    /// Builds the test list from test cases the caller already knows, without
    /// executing any test binary.
    ///
    /// `known_tests` needs an entry for every input binary that
    /// `options.platform_filter` keeps, and none for a binary that isn't an
    /// input. See [`TestList::new_with_known_tests`] for what happens if a test
    /// case isn't in its binary.
    pub fn build_with_known_tests(
        ctx: &'a SessionContext,
        profile: &'a EvaluatableProfile<'a>,
        inputs: SessionInputs<'a>,
        mut known_tests: BTreeMap<RustBinaryId, IdOrdMap<UnfilteredTestCase>>,
        test_filter: &TestFilter,
        options: KnownTestListOptions<'_>,
    ) -> Result<Self, KnownTestsBuildError> {
        let input_binary_ids: BTreeSet<&RustBinaryId> = inputs
            .binary_list
            .rust_binaries
            .iter()
            .map(|binary| &binary.id)
            .collect();
        let unexpected: Vec<RustBinaryId> = known_tests
            .keys()
            .filter(|binary_id| !input_binary_ids.contains(binary_id))
            .cloned()
            .collect();

        let prepared = PreparedInputs::new(inputs, options.platform_filter)?;

        let mut missing = Vec::new();
        let mut artifacts = Vec::with_capacity(prepared.artifacts.len());
        for artifact in prepared.artifacts {
            match known_tests.remove(&artifact.binary_id) {
                Some(test_cases) => artifacts.push((artifact, test_cases)),
                None => missing.push(artifact.binary_id),
            }
        }
        if !missing.is_empty() || !unexpected.is_empty() {
            return Err(KnownTestsBuildError::BinaryMismatch {
                missing,
                unexpected,
            });
        }

        let test_list = TestList::new_with_known_tests(
            artifacts,
            prepared.rust_build_meta,
            test_filter,
            options.partitioner_builder,
            prepared.workspace_root,
            prepared.env,
            profile,
            options.filter_bound,
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

/// A session's inputs, with paths remapped and binaries converted into
/// artifacts.
struct PreparedInputs<'a> {
    artifacts: Vec<RustTestArtifact<'a>>,
    rust_build_meta: RustBuildMeta<TestListState>,
    workspace_root: Utf8PathBuf,
    env: EnvironmentMap,
}

impl<'a> PreparedInputs<'a> {
    fn new(
        inputs: SessionInputs<'a>,
        platform_filter: Option<BuildPlatform>,
    ) -> Result<Self, FromMessagesError> {
        let SessionInputs {
            binary_list,
            packages,
            workspace_root,
            env,
            path_mapper,
        } = inputs;

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
            platform_filter,
        )?;

        Ok(Self {
            artifacts,
            rust_build_meta,
            workspace_root,
            env,
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use camino_tempfile::Utf8TempDir;
    use guppy::PackageId;
    use iddqd::id_ord_map;
    use nextest_filtering::ParseContext;
    use nextest_metadata::{
        FilterMatch, MismatchReason, RustTestBinaryKind, RustTestKind, TestCaseName,
    };
    use nextest_runner::{
        config::core::NextestConfig,
        list::{BinaryList, PackageInfo, RustTestBinary, TestBinaryInvocation},
        platform::BuildPlatforms,
        reuse_build::PathMapper,
        run_mode::NextestRunMode,
        test_filter::{FilterBound, RunIgnored, TestFilterPatterns},
    };
    use quick_junit::ReportUuid;
    use semver::Version;
    use std::sync::Arc;

    const PACKAGE_ID: &str = "root//:package";

    struct Setup {
        dir: Utf8TempDir,
        config: NextestConfig,
        build_platforms: BuildPlatforms,
        packages: IdOrdMap<PackageInfo>,
        ctx: SessionContext,
        filter: TestFilter,
    }

    impl Setup {
        fn new() -> Self {
            let dir = Utf8TempDir::new().expect("created temp dir");
            let config = NextestConfig::from_sources(
                dir.path(),
                &ParseContext::without_graph(),
                None,
                &[][..],
                &Default::default(),
            )
            .expect("the default config is valid");
            let build_platforms =
                BuildPlatforms::new_with_no_target().expect("the host platform is detected");

            let mut packages = IdOrdMap::new();
            packages.insert_overwrite(PackageInfo {
                id: PackageId::new(PACKAGE_ID),
                name: "package".to_owned(),
                version: Version::new(0, 0, 0),
                authors: Vec::new(),
                description: None,
                homepage: None,
                license: None,
                license_file: None,
                repository: None,
                minimum_rust_version: None,
                manifest_path: dir.path().join("BUCK"),
            });

            let filter = TestFilter::new(
                NextestRunMode::Test,
                RunIgnored::Default,
                TestFilterPatterns::default(),
                Vec::new(),
            )
            .expect("the filter is valid");

            Self {
                dir,
                config,
                build_platforms,
                packages,
                ctx: SessionContext::simple(ReportUuid::new_v4(), Version::new(0, 0, 0)),
                filter,
            }
        }

        fn profile(&self) -> EvaluatableProfile<'_> {
            self.config
                .profile(NextestConfig::DEFAULT_PROFILE)
                .expect("the default profile exists")
                .apply_build_platforms(&self.build_platforms)
        }

        /// Returns inputs whose binaries do not exist, so that building a
        /// session from them fails if anything tries to execute one.
        fn inputs(&self, binaries: &[(&str, BuildPlatform)]) -> SessionInputs<'_> {
            let root = self.dir.path();
            let rust_binaries = binaries
                .iter()
                .map(|&(name, build_platform)| RustTestBinary {
                    id: RustBinaryId::new(name),
                    path: nonexistent_path(root, name),
                    package_id: PACKAGE_ID.to_owned(),
                    kind: RustTestBinaryKind::TEST,
                    name: name.to_owned(),
                    build_platform,
                    invocation: TestBinaryInvocation::empty(),
                })
                .collect();

            SessionInputs {
                binary_list: Arc::new(BinaryList {
                    rust_build_meta: RustBuildMeta::new(root, root, self.build_platforms.clone()),
                    rust_binaries,
                }),
                packages: &self.packages,
                workspace_root: root.to_owned(),
                env: EnvironmentMap::empty(),
                path_mapper: PathMapper::noop(),
            }
        }
    }

    fn nonexistent_path(root: &Utf8Path, name: &str) -> Utf8PathBuf {
        let path = root.join("does-not-exist").join(name);
        assert!(!path.exists(), "{path} must not exist");
        path
    }

    fn options(platform_filter: Option<BuildPlatform>) -> KnownTestListOptions<'static> {
        KnownTestListOptions {
            partitioner_builder: None,
            platform_filter,
            filter_bound: FilterBound::All,
        }
    }

    fn test_case(name: &str, ignored: bool) -> UnfilteredTestCase {
        UnfilteredTestCase {
            name: TestCaseName::new(name),
            kind: RustTestKind::TEST,
            ignored,
        }
    }

    #[test]
    fn known_tests_are_filtered_without_executing_binaries() {
        let setup = Setup::new();
        let profile = setup.profile();

        let session = TestSession::build_with_known_tests(
            &setup.ctx,
            &profile,
            setup.inputs(&[("binary", BuildPlatform::Target)]),
            BTreeMap::from([(
                RustBinaryId::new("binary"),
                id_ord_map! { test_case("runs", false), test_case("is_ignored", true) },
            )]),
            &setup.filter,
            options(None),
        )
        .expect("the session builds without executing the binary");

        let filter_matches: BTreeMap<&str, FilterMatch> = session
            .test_list()
            .iter_tests()
            .map(|test| (test.name.as_str(), test.test_info.filter_match))
            .collect();
        assert_eq!(
            filter_matches,
            BTreeMap::from([
                (
                    "is_ignored",
                    FilterMatch::Mismatch {
                        reason: MismatchReason::Ignored,
                    },
                ),
                ("runs", FilterMatch::Matches),
            ]),
        );
    }

    #[test]
    fn known_tests_must_name_exactly_the_input_binaries() {
        let setup = Setup::new();
        let profile = setup.profile();

        let error = TestSession::build_with_known_tests(
            &setup.ctx,
            &profile,
            setup.inputs(&[
                ("covered", BuildPlatform::Target),
                ("uncovered", BuildPlatform::Target),
            ]),
            BTreeMap::from([
                (
                    RustBinaryId::new("covered"),
                    id_ord_map! { test_case("runs", false) },
                ),
                (RustBinaryId::new("not-an-input"), IdOrdMap::new()),
            ]),
            &setup.filter,
            options(None),
        )
        .err()
        .expect("the known tests do not match the inputs");

        match error {
            KnownTestsBuildError::BinaryMismatch {
                missing,
                unexpected,
            } => {
                assert_eq!(missing, vec![RustBinaryId::new("uncovered")]);
                assert_eq!(unexpected, vec![RustBinaryId::new("not-an-input")]);
            }
            other => panic!("expected a binary mismatch, got {other:?}"),
        }
    }

    /// A binary the platform filter drops is never listed, so it needs no
    /// known tests. It is still an input, though, so giving it some is not an
    /// error either.
    #[test]
    fn platform_filtered_binaries_need_no_known_tests() {
        let setup = Setup::new();
        let profile = setup.profile();
        let binaries = [
            ("target", BuildPlatform::Target),
            ("host", BuildPlatform::Host),
        ];

        let without_host = TestSession::build_with_known_tests(
            &setup.ctx,
            &profile,
            setup.inputs(&binaries),
            BTreeMap::from([(
                RustBinaryId::new("target"),
                id_ord_map! { test_case("runs", false) },
            )]),
            &setup.filter,
            options(Some(BuildPlatform::Target)),
        )
        .expect("the filtered-out binary needs no known tests");
        assert_eq!(without_host.test_list().test_count(), 1);

        let with_host = TestSession::build_with_known_tests(
            &setup.ctx,
            &profile,
            setup.inputs(&binaries),
            BTreeMap::from([
                (
                    RustBinaryId::new("target"),
                    id_ord_map! { test_case("runs", false) },
                ),
                (
                    RustBinaryId::new("host"),
                    id_ord_map! { test_case("also_runs", false) },
                ),
            ]),
            &setup.filter,
            options(Some(BuildPlatform::Target)),
        )
        .expect("known tests for a filtered-out input are allowed");
        assert_eq!(with_host.test_list().test_count(), 1);
    }
}
