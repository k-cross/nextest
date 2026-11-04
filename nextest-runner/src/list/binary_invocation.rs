// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Extra details for invoking a test binary.
//!
//! Cargo builds a test binary that nextest invokes directly: the program is the
//! binary path, and the only arguments are the libtest ones nextest appends
//! (`--list --format terse` when listing, `--exact <name> --nocapture` when
//! running).
//!
//! Other build systems describe a test target as a full argument vector plus a
//! set of environment variables. Buck2, for instance, hands its test executor
//! an `ExternalRunnerTestInfo` whose `command` may place a launcher in front of
//! the binary. This type carries that extra material so the libtest
//! arguments can still be appended to the end.

use nextest_metadata::RustBinaryId;
use std::collections::BTreeMap;
use tracing::warn;

/// Extra details for invoking a test binary, beyond its path.
///
/// The program is always the test suite's binary path; this type carries what
/// goes around it, such as a launcher's arguments, so that nextest's libtest
/// arguments still come last. Cargo produces an empty value, for which
/// nextest's behavior is exactly as though this type did not exist.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TestBinaryInvocation {
    /// Arguments placed between the binary path and nextest's libtest
    /// arguments.
    ///
    /// These are positioned so that the libtest arguments remain last, which is
    /// what a libtest-compatible harness expects.
    pub leading_args: Vec<String>,

    /// Environment variables applied in both the list and run phases.
    ///
    /// These take priority over Cargo's `[env]` configuration, but are
    /// overridden by wrapper script environments and by the variables nextest
    /// sets for every test process. A value for the dynamic library search
    /// path is kept, with the build's library directories placed ahead of it.
    pub env: BTreeMap<String, String>,
}

impl TestBinaryInvocation {
    /// Creates an invocation with no extra arguments or environment.
    ///
    /// This is what Cargo-built test binaries use.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Returns true if there is nothing to apply beyond the binary path.
    pub fn is_empty(&self) -> bool {
        self.leading_args.is_empty() && self.env.is_empty()
    }

    /// Warns if this invocation is about to be dropped on the way to a
    /// serializable summary.
    ///
    /// `nextest-metadata`'s summary types have no place for an invocation, so
    /// anything written out -- an archive, a recording, `--binaries-metadata`
    /// -- comes back with [`TestBinaryInvocation::empty`]. Giving them one is a
    /// breaking change to that crate's stable API, so it waits for a release
    /// that can bump the version.
    ///
    /// Cargo-built binaries have nothing to carry, so Cargo never reaches this.
    /// A build system integration that serializes a test list does, and its
    /// binaries would come back missing their leading arguments and
    /// environment -- so this warns rather than dropping them
    /// silently. It is a warning and not an assertion because the input is
    /// ordinary user data, which a frontend is free to supply.
    pub(crate) fn warn_if_unrepresentable(&self, binary_id: &RustBinaryId) {
        if self.is_empty() {
            return;
        }
        warn!(
            "invocation details for `{binary_id}` are not serialized, so a test list read \
             back from this output will invoke it without them"
        );
    }
}
