// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Mapping a finished run to an outcome.

use nextest_metadata::NextestExitCode;
use nextest_runner::{
    helpers::plural,
    reporter::events::{FinalRunStats, RunStats, RunStatsFailureKind},
    run_mode::NextestRunMode,
};
use tracing::{info, warn};

/// How to treat a run in which no tests were selected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NoTestsBehavior {
    /// Succeed.
    Pass,

    /// Succeed, with a warning.
    Warn,

    /// Fail.
    Fail,

    /// Fail, except on a rerun, where outstanding tests decide instead.
    Auto,
}

/// Why a run that executed did not succeed.
///
/// This is the shared truth about the outcome; each frontend maps it onto its
/// own error type or exit-code convention. [`exit_code`](Self::exit_code)
/// carries the canonical code so the mapping cannot drift between them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunFailure {
    /// No tests were selected to run.
    NoTestsRun {
        /// Whether the run was in test or benchmark mode.
        mode: NextestRunMode,

        /// Whether this outcome came from the default behavior rather than an
        /// explicit request to fail.
        is_default: bool,
    },

    /// A setup script failed.
    SetupScriptFailed,

    /// At least one test failed, or the run was cancelled after a failure.
    TestRunFailed {
        /// Whether a recording is available to rerun from.
        rerun_available: bool,
    },

    /// A rerun finished without seeing all of the outstanding tests.
    RerunTestsOutstanding {
        /// How many outstanding tests were not seen.
        count: usize,

        /// Whether a recording is available to rerun from.
        rerun_available: bool,
    },
}

impl RunFailure {
    /// Returns the process exit code for this failure.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::NoTestsRun { .. } => NextestExitCode::NO_TESTS_RUN,
            Self::SetupScriptFailed => NextestExitCode::SETUP_SCRIPT_FAILED,
            Self::TestRunFailed { .. } => NextestExitCode::TEST_RUN_FAILED,
            Self::RerunTestsOutstanding { .. } => NextestExitCode::RERUN_TESTS_OUTSTANDING,
        }
    }
}

/// Determines the final outcome of a test run.
///
/// A `no_tests` of `None` behaves as [`NoTestsBehavior::Auto`] but is reported
/// as the default rather than a choice. `outstanding_not_seen_count` is `Some`
/// exactly when the run was a rerun; `rerun_available` says whether this run
/// was recorded, so a failure can point at rerunning it.
pub fn final_outcome(
    mode: NextestRunMode,
    run_stats: RunStats,
    no_tests: Option<NoTestsBehavior>,
    outstanding_not_seen_count: Option<usize>,
    rerun_available: bool,
) -> Result<(), RunFailure> {
    let final_stats = run_stats.summarize_final();
    let is_rerun = outstanding_not_seen_count.is_some();

    // Handle the no-tests-run case first.
    if matches!(final_stats, FinalRunStats::NoTestsRun) {
        match no_tests {
            Some(NoTestsBehavior::Pass) => return Ok(()),
            Some(NoTestsBehavior::Warn) => {
                warn!("no {} to run", plural::tests_plural(mode));
                return Ok(());
            }
            Some(NoTestsBehavior::Fail) => {
                return Err(RunFailure::NoTestsRun {
                    mode,
                    is_default: false,
                });
            }
            // For reruns, `Auto` and the default check outstanding tests
            // below. For everything else they fail.
            Some(NoTestsBehavior::Auto) => {
                if !is_rerun {
                    return Err(RunFailure::NoTestsRun {
                        mode,
                        is_default: false,
                    });
                }
            }
            None => {
                if !is_rerun {
                    return Err(RunFailure::NoTestsRun {
                        mode,
                        is_default: true,
                    });
                }
            }
        }
    } else if let Some(failure) = failure_for_stats(final_stats, mode, rerun_available) {
        // Tests ran, and the run failed.
        return Err(failure);
    }

    // The run succeeded (or no tests ran on a rerun). Check for outstanding
    // tests.
    match outstanding_not_seen_count {
        Some(0) => {
            info!("no outstanding tests remain");
            Ok(())
        }
        Some(count) => Err(RunFailure::RerunTestsOutstanding {
            count,
            rerun_available,
        }),
        None => Ok(()),
    }
}

/// Converts final run statistics to a failure, if the run failed.
///
/// For `NoTestsRun` this always reports `is_default: true`; [`final_outcome`]
/// handles [`NoTestsBehavior`] before it gets here.
fn failure_for_stats(
    final_stats: FinalRunStats,
    mode: NextestRunMode,
    rerun_available: bool,
) -> Option<RunFailure> {
    match final_stats {
        FinalRunStats::Success => None,
        FinalRunStats::NoTestsRun => Some(RunFailure::NoTestsRun {
            mode,
            is_default: true,
        }),
        FinalRunStats::Cancelled {
            kind: RunStatsFailureKind::SetupScript,
            ..
        }
        | FinalRunStats::Failed {
            kind: RunStatsFailureKind::SetupScript,
        } => Some(RunFailure::SetupScriptFailed),
        FinalRunStats::Cancelled {
            kind: RunStatsFailureKind::Test { .. },
            ..
        }
        | FinalRunStats::Failed {
            kind: RunStatsFailureKind::Test { .. },
        } => Some(RunFailure::TestRunFailed { rerun_available }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_run_stats(initial_run_count: usize, finished_count: usize, passed: usize) -> RunStats {
        RunStats {
            initial_run_count,
            finished_count,
            passed,
            ..Default::default()
        }
    }

    #[test]
    fn no_tests_pass_succeeds() {
        let stats = make_run_stats(0, 0, 0);
        let result = final_outcome(
            NextestRunMode::Test,
            stats,
            Some(NoTestsBehavior::Pass),
            None,
            false,
        );
        assert!(result.is_ok(), "no-tests=pass should succeed");
    }

    #[test]
    fn no_tests_warn_succeeds() {
        let stats = make_run_stats(0, 0, 0);
        let result = final_outcome(
            NextestRunMode::Test,
            stats,
            Some(NoTestsBehavior::Warn),
            None,
            false,
        );
        assert!(result.is_ok(), "no-tests=warn should succeed");
    }

    #[test]
    fn no_tests_fail_fails() {
        let stats = make_run_stats(0, 0, 0);
        let result = final_outcome(
            NextestRunMode::Test,
            stats,
            Some(NoTestsBehavior::Fail),
            None,
            false,
        );
        assert_eq!(
            result,
            Err(RunFailure::NoTestsRun {
                mode: NextestRunMode::Test,
                is_default: false,
            }),
            "no-tests=fail should fail"
        );
    }

    #[test]
    fn no_tests_auto_fails_outside_a_rerun() {
        let stats = make_run_stats(0, 0, 0);
        let result = final_outcome(
            NextestRunMode::Test,
            stats,
            Some(NoTestsBehavior::Auto),
            None,
            false,
        );
        assert_eq!(
            result,
            Err(RunFailure::NoTestsRun {
                mode: NextestRunMode::Test,
                is_default: false,
            }),
            "no-tests=auto (not a rerun) should fail"
        );
    }

    #[test]
    fn no_tests_auto_defers_to_outstanding_on_a_rerun() {
        let stats = make_run_stats(0, 0, 0);
        let result = final_outcome(
            NextestRunMode::Test,
            stats,
            Some(NoTestsBehavior::Auto),
            Some(5),
            false,
        );
        assert_eq!(
            result,
            Err(RunFailure::RerunTestsOutstanding {
                count: 5,
                rerun_available: false,
            }),
            "no-tests=auto (rerun with outstanding) should report outstanding tests"
        );

        let stats = make_run_stats(0, 0, 0);
        let result = final_outcome(
            NextestRunMode::Test,
            stats,
            Some(NoTestsBehavior::Auto),
            Some(0),
            false,
        );
        assert!(
            result.is_ok(),
            "no-tests=auto (rerun, no outstanding) should succeed"
        );
    }

    #[test]
    fn the_default_matches_auto_but_reports_as_default() {
        let stats = make_run_stats(0, 0, 0);
        let result = final_outcome(NextestRunMode::Test, stats, None, None, false);
        assert_eq!(
            result,
            Err(RunFailure::NoTestsRun {
                mode: NextestRunMode::Test,
                is_default: true,
            }),
            "the default (not a rerun) should fail with is_default: true"
        );

        let stats = make_run_stats(0, 0, 0);
        let result = final_outcome(NextestRunMode::Test, stats, None, Some(3), false);
        assert_eq!(
            result,
            Err(RunFailure::RerunTestsOutstanding {
                count: 3,
                rerun_available: false,
            }),
            "the default (rerun with outstanding) should report outstanding tests"
        );
    }

    #[test]
    fn a_passing_run_succeeds() {
        let stats = make_run_stats(5, 5, 5);
        let result = final_outcome(NextestRunMode::Test, stats, None, None, false);
        assert!(result.is_ok(), "all tests passed should succeed");

        let result = final_outcome(NextestRunMode::Test, stats, None, Some(0), false);
        assert!(
            result.is_ok(),
            "all tests passed (rerun, no outstanding) should succeed"
        );
    }

    #[test]
    fn a_passing_rerun_with_outstanding_tests_fails() {
        let stats = make_run_stats(5, 5, 5);
        let result = final_outcome(NextestRunMode::Test, stats, None, Some(2), false);
        assert_eq!(
            result,
            Err(RunFailure::RerunTestsOutstanding {
                count: 2,
                rerun_available: false,
            }),
        );

        // With a recording, the failure can point at continuing the rerun.
        let result = final_outcome(NextestRunMode::Test, stats, None, Some(2), true);
        assert_eq!(
            result,
            Err(RunFailure::RerunTestsOutstanding {
                count: 2,
                rerun_available: true,
            }),
        );
    }

    #[test]
    fn test_failures_fail_the_run() {
        let mut stats = make_run_stats(5, 5, 3);
        stats.failed = 2;
        let result = final_outcome(NextestRunMode::Test, stats, None, None, false);
        assert_eq!(
            result,
            Err(RunFailure::TestRunFailed {
                rerun_available: false
            }),
        );

        let result = final_outcome(NextestRunMode::Test, stats, None, None, true);
        assert_eq!(
            result,
            Err(RunFailure::TestRunFailed {
                rerun_available: true
            }),
        );
    }

    #[test]
    fn test_failures_take_precedence_over_outstanding_tests() {
        let mut stats = make_run_stats(5, 5, 3);
        stats.failed = 2;
        let result = final_outcome(NextestRunMode::Test, stats, None, Some(10), false);
        assert_eq!(
            result,
            Err(RunFailure::TestRunFailed {
                rerun_available: false
            }),
        );
    }

    #[test]
    fn setup_script_failures_are_distinct_from_test_failures() {
        for final_stats in [
            FinalRunStats::Failed {
                kind: RunStatsFailureKind::SetupScript,
            },
            FinalRunStats::Cancelled {
                reason: None,
                kind: RunStatsFailureKind::SetupScript,
            },
        ] {
            assert_eq!(
                failure_for_stats(final_stats, NextestRunMode::Test, false),
                Some(RunFailure::SetupScriptFailed),
                "{final_stats:?} is a setup script failure"
            );
        }

        for final_stats in [
            FinalRunStats::Failed {
                kind: RunStatsFailureKind::Test {
                    initial_run_count: 5,
                    not_run: 0,
                },
            },
            FinalRunStats::Cancelled {
                reason: None,
                kind: RunStatsFailureKind::Test {
                    initial_run_count: 5,
                    not_run: 2,
                },
            },
        ] {
            assert_eq!(
                failure_for_stats(final_stats, NextestRunMode::Test, true),
                Some(RunFailure::TestRunFailed {
                    rerun_available: true
                }),
                "{final_stats:?} is a test failure"
            );
        }
    }

    #[test]
    fn exit_codes_match_the_canonical_constants() {
        assert_eq!(
            RunFailure::NoTestsRun {
                mode: NextestRunMode::Test,
                is_default: true,
            }
            .exit_code(),
            NextestExitCode::NO_TESTS_RUN,
        );
        assert_eq!(
            RunFailure::SetupScriptFailed.exit_code(),
            NextestExitCode::SETUP_SCRIPT_FAILED,
        );
        assert_eq!(
            RunFailure::TestRunFailed {
                rerun_available: false
            }
            .exit_code(),
            NextestExitCode::TEST_RUN_FAILED,
        );
        assert_eq!(
            RunFailure::RerunTestsOutstanding {
                count: 1,
                rerun_available: false,
            }
            .exit_code(),
            NextestExitCode::RERUN_TESTS_OUTSTANDING,
        );
    }
}
