// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Errors produced by the session pipeline.
//!
//! Each phase returns its natural error type rather than one merged enum, so
//! that a frontend can keep its own rendering and exit-code policy for each.

use camino::Utf8PathBuf;
use nextest_runner::errors::{
    ConfigureHandleInheritanceError, CreateTestListError, FromMessagesError,
    TestRunnerExecuteErrors, WriteEventError,
};
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
    fn absent_report_errors_stay_absent() {
        let errors: TestRunnerExecuteErrors<SinkError<Infallible>> = TestRunnerExecuteErrors {
            report_error: None,
            join_errors: Vec::new(),
        };
        let mapped = into_report_errors(errors);
        assert!(mapped.report_error.is_none());
    }
}
