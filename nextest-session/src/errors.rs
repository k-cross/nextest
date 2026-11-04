// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Errors produced by the session pipeline.
//!
//! Each phase returns its natural error type rather than one merged enum, so
//! that a frontend can keep its own rendering and exit-code policy for each.

pub use nextest_filtering::errors::FiltersetParseErrors;
use nextest_metadata::RustBinaryId;
pub use nextest_runner::errors::{
    ConfigParseError, ConfigureHandleInheritanceError, CreateTestListError, FromMessagesError,
    ProfileNotFound, TestFilterBuildError, TestRunnerBuildError, TestRunnerExecuteErrors,
    WriteEventError, WriteTestListError,
};
use nextest_runner::helpers::plural;
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
    /// Building the test list failed.
    #[error(transparent)]
    Session(#[from] SessionBuildError),

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

/// An error executing a run to completion.
#[derive(Debug, Error)]
pub enum ExecuteError {
    /// Configuring how file handles are inherited failed.
    #[error(transparent)]
    ConfigureHandleInheritance(ConfigureHandleInheritanceError),

    /// The runner failed to execute tests or to report their results.
    #[error(transparent)]
    Execute(TestRunnerExecuteErrors<WriteEventError>),
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
