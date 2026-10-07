// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Errors produced by the session pipeline.
//!
//! Each phase returns its natural error type rather than one merged enum, so
//! that a frontend can keep its own rendering and exit-code policy for each.

use camino::Utf8PathBuf;
pub use nextest_filtering::errors::FiltersetParseErrors;
use nextest_metadata::RustBinaryId;
pub use nextest_runner::errors::{
    ConfigParseError, ConfigureHandleInheritanceError, CreateTestListError, FromMessagesError,
    ProfileNotFound, TestFilterBuildError, TestRunnerBuildError, TestRunnerExecuteErrors,
    WriteEventError, WriteTestListError,
};
use nextest_runner::helpers::plural;
use std::{convert::Infallible, fmt, io};
use thiserror::Error;

/// An error building a test list from a session's inputs.
#[derive(Debug, Error)]
pub enum SessionBuildError {
    /// Converting the binary list into test artifacts failed.
    #[error(transparent)]
    FromMessages(#[from] FromMessagesError),

    /// Building the test list failed.
    #[error(transparent)]
    CreateTestList(#[from] CreateTestListError),
}

/// An error building a test list from test cases the caller already knows.
#[derive(Debug, Error)]
pub enum KnownTestsBuildError {
    /// Converting the binary list into test artifacts failed.
    #[error(transparent)]
    FromMessages(#[from] FromMessagesError),

    /// Building the test list failed.
    #[error(transparent)]
    CreateTestList(#[from] CreateTestListError),

    /// The known test cases were not given for exactly the input binaries.
    #[error(
        "match known test cases to the binary list ({})",
        describe_binary_mismatch(.missing, .unexpected)
    )]
    BinaryMismatch {
        /// Binaries in the inputs that have no known test cases.
        missing: Vec<RustBinaryId>,

        /// Binaries with known test cases that are not in the inputs.
        unexpected: Vec<RustBinaryId>,
    },
}

fn describe_binary_mismatch(missing: &[RustBinaryId], unexpected: &[RustBinaryId]) -> String {
    let describe = |binary_ids: &[RustBinaryId]| {
        let names = binary_ids
            .iter()
            .map(|binary_id| format!("`{binary_id}`"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{} {names}", plural::binaries_str(binary_ids.len()))
    };

    let mut parts = Vec::with_capacity(2);
    if !missing.is_empty() {
        parts.push(format!("missing for {}", describe(missing)));
    }
    if !unexpected.is_empty() {
        parts.push(format!("given for unknown {}", describe(unexpected)));
    }
    parts.join("; ")
}

/// An error creating the store directory a profile's reports go in.
#[derive(Debug, Error)]
#[error("failed to create store directory `{store_dir}`")]
pub struct StoreDirCreateError {
    /// The directory that could not be created.
    pub store_dir: Utf8PathBuf,

    /// The underlying error.
    #[source]
    pub error: io::Error,
}

/// Why a run's event callback failed.
///
/// The two arms are kept apart so the message says whether the run stopped
/// because results could not be delivered or because they could not be
/// rendered.
#[derive(Debug, Error)]
pub enum SinkError<E: fmt::Debug> {
    /// Forwarding an event to the caller's sink failed.
    #[error("failed to forward results: {0:?}")]
    Sink(E),

    /// Writing an event to the reporter failed.
    #[error(transparent)]
    Report(WriteEventError),
}

impl SinkError<Infallible> {
    /// With an infallible sink, only the reporter can fail.
    pub fn into_report_error(self) -> WriteEventError {
        match self {
            Self::Sink(infallible) => match infallible {},
            Self::Report(error) => error,
        }
    }
}

/// An error executing a run to completion.
#[derive(Debug, Error)]
pub enum ExecuteError<E: fmt::Debug> {
    /// Configuring how file handles are inherited failed.
    #[error(transparent)]
    ConfigureHandleInheritance(ConfigureHandleInheritanceError),

    /// The runner failed to execute tests or to report their results.
    #[error(transparent)]
    Execute(TestRunnerExecuteErrors<SinkError<E>>),
}

/// Maps [`TestRunnerExecuteErrors`] over an infallible sink back to the
/// reporter's own error type.
///
/// This lets a frontend with no sink keep handling the error type it would
/// see driving the runner directly.
pub fn into_report_errors(
    errors: TestRunnerExecuteErrors<SinkError<Infallible>>,
) -> TestRunnerExecuteErrors<WriteEventError> {
    TestRunnerExecuteErrors {
        report_error: errors.report_error.map(SinkError::into_report_error),
        join_errors: errors.join_errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_errors_survive_the_infallible_mapping() {
        let errors = TestRunnerExecuteErrors {
            report_error: Some(SinkError::<Infallible>::Report(WriteEventError::Io(
                io::Error::other("disk full"),
            ))),
            join_errors: Vec::new(),
        };
        let mapped = into_report_errors(errors);
        assert!(
            matches!(&mapped.report_error, Some(WriteEventError::Io(error)) if error.to_string() == "disk full"),
            "the reporter error passes through unchanged, got {:?}",
            mapped.report_error
        );
        assert!(mapped.join_errors.is_empty());
    }

    #[test]
    fn binary_mismatch_names_every_binary() {
        let id = |name: &str| RustBinaryId::new(name);
        let message = |missing: Vec<RustBinaryId>, unexpected: Vec<RustBinaryId>| {
            KnownTestsBuildError::BinaryMismatch {
                missing,
                unexpected,
            }
            .to_string()
        };

        assert_eq!(
            message(vec![id("a"), id("b")], Vec::new()),
            "match known test cases to the binary list (missing for binaries `a`, `b`)"
        );
        assert_eq!(
            message(Vec::new(), vec![id("c")]),
            "match known test cases to the binary list (given for unknown binary `c`)"
        );
        assert_eq!(
            message(vec![id("a")], vec![id("c"), id("d")]),
            "match known test cases to the binary list \
             (missing for binary `a`; given for unknown binaries `c`, `d`)"
        );
    }

    #[test]
    fn absent_report_errors_stay_absent() {
        let errors: TestRunnerExecuteErrors<SinkError<Infallible>> = TestRunnerExecuteErrors {
            report_error: None,
            join_errors: Vec::new(),
        };
        let mapped = into_report_errors(errors);
        assert!(mapped.report_error.is_none());
    }
}
