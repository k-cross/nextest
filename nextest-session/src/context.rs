// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The identity and environment a session runs under.

use nextest_runner::{
    double_spawn::DoubleSpawnInfo, list::TestExecuteContext, runner::VersionEnvVars,
    target_runner::TargetRunner,
};
use quick_junit::ReportUuid;
use semver::Version;

/// The identity and environment shared by every phase of a session.
///
/// One value covers both listing and execution, so that a test binary sees the
/// same `NEXTEST_RUN_ID` in both phases and can key state on it across them.
#[derive(Debug)]
pub struct SessionContext {
    /// The ID that identifies this run.
    pub run_id: ReportUuid,

    /// Version-related environment variables exposed to test processes.
    pub version_env_vars: VersionEnvVars,

    /// Double-spawn info, for the `SIGTSTP` race avoidance Cargo builds use.
    pub double_spawn: DoubleSpawnInfo,

    /// A runner for the target platform, if one is configured.
    pub target_runner: TargetRunner,
}

impl SessionContext {
    /// Returns a context with no version constraints, no double-spawning, and
    /// no target runner.
    ///
    /// This is the right shape for an integration that has no configuration
    /// sources for those features.
    pub fn simple(run_id: ReportUuid, current_version: Version) -> Self {
        Self {
            run_id,
            version_env_vars: VersionEnvVars {
                current_version,
                required_version: None,
                recommended_version: None,
            },
            double_spawn: DoubleSpawnInfo::disabled(),
            target_runner: TargetRunner::empty(),
        }
    }

    /// Returns the context both the list and run phases hand to test processes.
    pub fn test_execute_context<'a>(&'a self, profile_name: &'a str) -> TestExecuteContext<'a> {
        TestExecuteContext {
            run_id: self.run_id,
            version_env_vars: &self.version_env_vars,
            profile_name,
            double_spawn: &self.double_spawn,
            target_runner: &self.target_runner,
        }
    }
}
