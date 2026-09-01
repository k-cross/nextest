// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Driving the `nextest-session` pipeline for one Buck2 target.
//!
//! The pipeline itself -- configuration, profiles, listing, running, and the
//! exit-code policy -- is `nextest-session`'s. This module supplies the inputs
//! and holds the pieces both modes need, so that listing a target and running
//! one of its tests read configuration the same way and agree about what the
//! tests are called.

use crate::{
    convert::Buck2BinaryList,
    errors::{ExpectedError, Result},
};
use camino::Utf8PathBuf;
use nextest_session::{
    ConfigExperimental, EarlyProfile, EnvironmentMap, EvaluatableProfile, FilterBound,
    ListProgressOptions, NextestConfig, ParseContext, PathMapper, SessionContext, SessionInputs,
    ShowProgress, ShowTerminalProgress, TestFilter, TestListOptions, TestSession, ThemeCharacters,
    WriteStr, errors::SessionBuildError, evaluate_profile, force_or_new_run_id,
};
use semver::Version;
use std::io::{self, IsTerminal, Write};

/// Everything both modes need to reach the pipeline.
#[derive(Debug)]
pub(crate) struct Context {
    /// The target's label, which is also its binary ID.
    pub(crate) label: String,

    /// The converted binary and its package.
    pub(crate) binaries: Buck2BinaryList,

    /// The Buck2 project root.
    pub(crate) project_root: Utf8PathBuf,

    /// The nextest profile to use.
    pub(crate) profile_name: Option<String>,

    /// A path to a nextest configuration file, if one was given.
    pub(crate) config_file: Option<Utf8PathBuf>,
}

impl Context {
    /// Loads nextest's configuration from the project root.
    pub(crate) fn load_config(&self) -> Result<NextestConfig> {
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

    /// Looks up the profile to run under.
    pub(crate) fn load_profile<'cfg>(
        &self,
        config: &'cfg NextestConfig,
    ) -> Result<EarlyProfile<'cfg>> {
        let name = self
            .profile_name
            .as_deref()
            .unwrap_or(NextestConfig::DEFAULT_PROFILE);
        config
            .profile(name)
            .map_err(|error| ExpectedError::ProfileNotFound { error })
    }

    /// Resolves the profile against the platforms it will run on.
    pub(crate) fn evaluate_profile<'cfg>(
        &self,
        early_profile: EarlyProfile<'cfg>,
    ) -> Result<EvaluatableProfile<'cfg>> {
        evaluate_profile(
            early_profile,
            &self.binaries.binary_list.rust_build_meta.build_platforms,
        )
        .map_err(|error| ExpectedError::StoreDirCreateError { error })
    }

    /// Builds the context a session runs under.
    ///
    /// `force_or_new_run_id` honours `NEXTEST_RUN_ID` when it is set. Buck2 runs
    /// every test in its own action rather than as part of one nextest run, so
    /// each invocation would otherwise mint an ID of its own; setting the
    /// variable on the rule's `env` is what makes a whole `buck2 test` share
    /// one.
    pub(crate) fn session_context(&self) -> SessionContext {
        SessionContext::simple(
            force_or_new_run_id(),
            Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version is valid semver"),
        )
    }

    /// Enumerates the binary's tests, producing a session to list or run.
    pub(crate) fn build_session<'a>(
        &'a self,
        ctx: &'a SessionContext,
        profile: &'a EvaluatableProfile<'a>,
        filter: &TestFilter,
        filter_bound: FilterBound,
    ) -> Result<TestSession<'a>> {
        TestSession::build(
            ctx,
            profile,
            SessionInputs {
                binary_list: self.binaries.binary_list.clone(),
                packages: &self.binaries.packages,
                workspace_root: self.project_root.clone(),
                env: EnvironmentMap::empty(),
                path_mapper: PathMapper::noop(),
            },
            filter,
            TestListOptions {
                partitioner_builder: None,
                platform_filter: None,
                filter_bound,
                list_threads: 1,
                progress: ListProgressOptions::new(
                    ShowProgress::default(),
                    ShowTerminalProgress::No,
                    ThemeCharacters::default(),
                    io::stderr().is_terminal(),
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

/// Writes the displayer's output to standard error, without any cursor control.
///
/// Buck2 captures each action's standard error and shows it alongside the
/// result, so this is the detail view for a test -- which is why it is written
/// without a progress bar or any other cursor control.
pub(crate) struct PlainStderrWriter(io::Stderr);

impl PlainStderrWriter {
    pub(crate) fn new() -> Self {
        Self(io::stderr())
    }
}

impl WriteStr for PlainStderrWriter {
    fn write_str(&mut self, s: &str) -> io::Result<()> {
        self.0.write_all(s.as_bytes())
    }

    fn write_str_flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}
