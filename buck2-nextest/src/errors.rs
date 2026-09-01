// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Error types for `buck2-nextest`.

use camino::{FromPathBufError, Utf8PathBuf};
use miette::Diagnostic;
use nextest_session::{
    NextestExitCode,
    errors::{
        ConfigParseError, ConfigureHandleInheritanceError, CreateTestListError, FromMessagesError,
        ProfileNotFound, StoreDirCreateError, TestFilterBuildError, TestRunnerBuildError,
        TestRunnerExecuteErrors, WriteEventError,
    },
    events::CancelReason,
};
use thiserror::Error;

/// The result type used throughout `buck2-nextest`.
pub type Result<T, E = ExpectedError> = std::result::Result<T, E>;

/// An error that is expected to occur in normal operation, and that carries an
/// exit code.
///
/// This mirrors `cargo-nextest`'s error model: these are user or environment
/// errors, as opposed to internal errors which panic.
#[derive(Debug, Diagnostic, Error)]
#[non_exhaustive]
pub enum ExpectedError {
    /// The current directory could not be determined.
    #[error("failed to determine the current directory")]
    CurrentDirError {
        /// The underlying error.
        #[source]
        error: std::io::Error,
    },

    /// The current directory was not valid UTF-8.
    #[error("the current directory is not valid UTF-8")]
    CurrentDirNonUtf8 {
        /// The underlying error, which carries the path.
        #[source]
        error: FromPathBufError,
    },

    /// The project root could not be made absolute.
    #[error("failed to make the project root `{path}` absolute")]
    ProjectRootAbsoluteError {
        /// The project root as it was given.
        path: Utf8PathBuf,
        /// The underlying error.
        #[source]
        error: std::io::Error,
    },

    /// The project root, made absolute, was not valid UTF-8.
    #[error("the project root `{path}` is not valid UTF-8 once made absolute")]
    ProjectRootNonUtf8 {
        /// The project root as it was given.
        path: Utf8PathBuf,
        /// The underlying error.
        #[source]
        error: FromPathBufError,
    },

    /// The host platform could not be determined.
    #[error("failed to detect the host platform")]
    HostPlatformDetectError {
        /// The underlying error.
        #[source]
        error: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Nextest's configuration could not be parsed.
    #[error("failed to parse nextest configuration")]
    ConfigParseError {
        /// The underlying error.
        #[source]
        error: ConfigParseError,
    },

    /// The requested profile was not found.
    #[error("profile not found")]
    ProfileNotFound {
        /// The underlying error.
        #[source]
        error: ProfileNotFound,
    },

    /// The test filter could not be constructed.
    #[error("failed to construct test filter")]
    TestFilterBuildError {
        /// The underlying error.
        #[source]
        error: TestFilterBuildError,
    },

    /// The binary list could not be converted into test artifacts.
    #[error("failed to build the list of test binaries")]
    FromMessagesError {
        /// The underlying error.
        #[source]
        error: FromMessagesError,
    },

    /// The test list could not be created.
    #[error("failed to list tests")]
    CreateTestListError {
        /// The underlying error.
        #[source]
        error: CreateTestListError,
    },

    /// The test runner could not be built.
    #[error("failed to set up the test runner")]
    TestRunnerBuildError {
        /// The underlying error.
        #[source]
        error: TestRunnerBuildError,
    },

    /// Creating the store directory for the profile's reports failed.
    #[error("failed to create the profile's store directory")]
    StoreDirCreateError {
        /// The underlying error.
        #[source]
        error: StoreDirCreateError,
    },

    /// The test Buck2 asked for was not in the binary.
    ///
    /// Buck2 only ever runs a test it saw in the listing, so this means the two
    /// disagree.
    #[error("no test named `{test_name}` in `{label}`")]
    #[diagnostic(help(
        "Buck2 runs the tests that `buck2-nextest list` reported for this target, so the \
         listing and this run disagree; this is usually a stale listing, which \
         `buck2 clean` clears"
    ))]
    TestNotFound {
        /// The target the test was expected in.
        label: String,
        /// The test name Buck2 asked for.
        test_name: String,
    },

    /// The JSON written for Buck2 could not be serialized.
    #[error("failed to serialize results for Buck2")]
    ResultSerializeError {
        /// The underlying error.
        #[source]
        error: serde_json::Error,
    },

    /// Writing reporter output failed.
    #[error("failed to write test output")]
    WriteEventError {
        /// The underlying error.
        #[source]
        error: std::io::Error,
    },

    /// Configuring how file handles are inherited failed.
    #[error("failed to configure handle inheritance")]
    ConfigureHandleInheritance {
        /// The underlying error.
        #[source]
        error: ConfigureHandleInheritanceError,
    },

    /// The runner failed to execute tests or to report their results.
    #[error("failed to execute tests")]
    TestRunExecuteError {
        /// The underlying errors.
        #[source]
        error: TestRunnerExecuteErrors<WriteEventError>,
    },

    /// The run ended before the test Buck2 asked for reported a result.
    ///
    /// The test was in the listing, so unlike [`Self::TestNotFound`] the two
    /// agree; something stopped the run first, and that cause is what the user
    /// needs to see.
    #[error("the run was cancelled ({reason}) before `{test_name}` in `{label}` reported a result")]
    #[diagnostic(help(
        "this is not a stale listing; look for the failure that cancelled the run above"
    ))]
    RunCancelled {
        /// The target the test was expected in.
        label: String,
        /// The test name Buck2 asked for.
        test_name: String,
        /// Why the run was cancelled.
        reason: CancelReason,
    },
}

impl ExpectedError {
    /// Returns the process exit code for this error.
    ///
    /// These match `cargo-nextest`'s codes so that tooling can treat the two
    /// binaries alike.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::CurrentDirError { .. }
            | Self::CurrentDirNonUtf8 { .. }
            | Self::ProjectRootAbsoluteError { .. }
            | Self::ProjectRootNonUtf8 { .. }
            | Self::HostPlatformDetectError { .. }
            | Self::ConfigParseError { .. }
            | Self::ProfileNotFound { .. }
            | Self::TestFilterBuildError { .. }
            | Self::TestRunnerBuildError { .. }
            | Self::StoreDirCreateError { .. } => NextestExitCode::SETUP_ERROR,
            Self::FromMessagesError { .. }
            | Self::CreateTestListError { .. }
            | Self::TestNotFound { .. } => NextestExitCode::TEST_LIST_CREATION_FAILED,
            Self::ConfigureHandleInheritance { .. } => NextestExitCode::SETUP_ERROR,
            Self::RunCancelled { reason, .. } => match reason {
                CancelReason::SetupScriptFailure => NextestExitCode::SETUP_SCRIPT_FAILED,
                _ => NextestExitCode::TEST_RUN_FAILED,
            },
            Self::ResultSerializeError { .. }
            | Self::WriteEventError { .. }
            | Self::TestRunExecuteError { .. } => NextestExitCode::WRITE_OUTPUT_ERROR,
        }
    }
}
