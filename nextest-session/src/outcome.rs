// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Mapping a finished run to an outcome.

use nextest_metadata::NextestExitCode;
use nextest_runner::{
    helpers::plural,
    reporter::events::{RunFailureStep, RunOutcome},
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
    outcome: RunOutcome,
    no_tests: Option<NoTestsBehavior>,
    outstanding_not_seen_count: Option<usize>,
    rerun_available: bool,
) -> Result<(), RunFailure> {
    let is_rerun = outstanding_not_seen_count.is_some();

    match outcome {
        RunOutcome::NoTestsRun => match no_tests {
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
        },
        RunOutcome::Success => {}
        RunOutcome::Cancelled { step } | RunOutcome::Failed { step } => {
            return Err(match step {
                RunFailureStep::SetupScript => RunFailure::SetupScriptFailed,
                RunFailureStep::Test => RunFailure::TestRunFailed { rerun_available },
            });
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use nextest_runner::reporter::events::{FinalRunStats, RunStats, RunStatsFailureKind};

    fn make_single_run(
        initial_run_count: usize,
        finished_count: usize,
        passed: usize,
        failed: usize,
    ) -> RunOutcome {
        RunOutcome::from_final_stats(
            RunStats {
                initial_run_count,
                finished_count,
                passed,
                failed,
                ..Default::default()
            }
            .summarize_final(),
        )
    }

    #[test]
    fn no_tests_pass_succeeds() {
        let outcome = make_single_run(0, 0, 0, 0);
        let result = final_outcome(
            NextestRunMode::Test,
            outcome,
            Some(NoTestsBehavior::Pass),
            None,
            false,
        );
        assert!(result.is_ok(), "no-tests=pass should succeed");
    }

    #[test]
    fn no_tests_warn_succeeds() {
        let outcome = make_single_run(0, 0, 0, 0);
        let result = final_outcome(
            NextestRunMode::Test,
            outcome,
            Some(NoTestsBehavior::Warn),
            None,
            false,
        );
        assert!(result.is_ok(), "no-tests=warn should succeed");
    }

    #[test]
    fn no_tests_fail_fails() {
        let outcome = make_single_run(0, 0, 0, 0);
        let result = final_outcome(
            NextestRunMode::Test,
            outcome,
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
        let outcome = make_single_run(0, 0, 0, 0);
        let result = final_outcome(
            NextestRunMode::Test,
            outcome,
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
        let outcome = make_single_run(0, 0, 0, 0);
        let result = final_outcome(
            NextestRunMode::Test,
            outcome,
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

        let outcome = make_single_run(0, 0, 0, 0);
        let result = final_outcome(
            NextestRunMode::Test,
            outcome,
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
        let outcome = make_single_run(0, 0, 0, 0);
        let result = final_outcome(NextestRunMode::Test, outcome, None, None, false);
        assert_eq!(
            result,
            Err(RunFailure::NoTestsRun {
                mode: NextestRunMode::Test,
                is_default: true,
            }),
            "the default (not a rerun) should fail with is_default: true"
        );

        let outcome = make_single_run(0, 0, 0, 0);
        let result = final_outcome(NextestRunMode::Test, outcome, None, Some(3), false);
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
        let outcome = make_single_run(5, 5, 5, 0);
        let result = final_outcome(NextestRunMode::Test, outcome, None, None, false);
        assert!(result.is_ok(), "all tests passed should succeed");

        let result = final_outcome(NextestRunMode::Test, outcome, None, Some(0), false);
        assert!(
            result.is_ok(),
            "all tests passed (rerun, no outstanding) should succeed"
        );
    }

    #[test]
    fn a_passing_rerun_with_outstanding_tests_fails() {
        let outcome = make_single_run(5, 5, 5, 0);
        let result = final_outcome(NextestRunMode::Test, outcome, None, Some(2), false);
        assert_eq!(
            result,
            Err(RunFailure::RerunTestsOutstanding {
                count: 2,
                rerun_available: false,
            }),
        );

        let result = final_outcome(NextestRunMode::Test, outcome, None, Some(2), true);
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
        let outcome = make_single_run(5, 5, 3, 2);
        let result = final_outcome(NextestRunMode::Test, outcome, None, None, false);
        assert_eq!(
            result,
            Err(RunFailure::TestRunFailed {
                rerun_available: false
            }),
        );

        let result = final_outcome(NextestRunMode::Test, outcome, None, None, true);
        assert_eq!(
            result,
            Err(RunFailure::TestRunFailed {
                rerun_available: true
            }),
        );
    }

    #[test]
    fn test_failures_take_precedence_over_outstanding_tests() {
        let outcome = make_single_run(5, 5, 3, 2);
        let result = final_outcome(NextestRunMode::Test, outcome, None, Some(10), false);
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
            let outcome = RunOutcome::from_final_stats(final_stats);
            assert_eq!(
                final_outcome(NextestRunMode::Test, outcome, None, None, false),
                Err(RunFailure::SetupScriptFailed),
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
            let outcome = RunOutcome::from_final_stats(final_stats);
            assert_eq!(
                final_outcome(NextestRunMode::Test, outcome, None, None, true),
                Err(RunFailure::TestRunFailed {
                    rerun_available: true
                }),
                "{final_stats:?} is a test failure"
            );
        }

        assert_eq!(
            final_outcome(
                NextestRunMode::Test,
                RunOutcome::Failed {
                    step: RunFailureStep::Test,
                },
                Some(NoTestsBehavior::Pass),
                None,
                false,
            ),
            Err(RunFailure::TestRunFailed {
                rerun_available: false,
            }),
            "--no-tests=pass doesn't mask a failed run"
        );
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
