// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A nextest client for Buck2's internal test runner.
//!
//! Buck2 discovers, schedules, and caches tests; nextest executes them. A rule
//! that returns `InternalRunnerTestInfo` names two commands, and Buck2 runs
//! both itself as ordinary actions: `listing_command` once per test target, to
//! find out what tests exist, and `command` once per discovered test, with the
//! test's filter appended as the final argument. This crate is the binary on
//! the far end of both.
//!
//! So there are two modes, and each is a single Buck2 action:
//!
//! * `list` enumerates the tests in one test binary and writes them as the
//!   JSON that Buck2's `parse_test_listing` callback reads.
//! * `run` executes exactly one test and writes the JSON that
//!   `parse_test_result` reads.
//!
//! The division of labour follows from that. Buck2 owns everything spanning
//! tests -- scheduling, concurrency, caching, retrying at the action level, and
//! the results UI -- because it is the one running the actions. Nextest owns
//! everything within a test, since the run mode drives the real
//! `nextest-session` pipeline: the profile and its configuration, per-test
//! retries, slow-test handling, leak detection, and the environment a test sees.
//!
//! # Why a whole nextest per test
//!
//! Running one test per process is what nextest does anyway, so the cost here is
//! the pipeline around it rather than the isolation. In exchange, a test behaves
//! identically whether it was run by Buck2 or by `cargo nextest run`, and the
//! nextest configuration in the repository keeps meaning what it says.
//!
//! Note that the run mode rebuilds the test list for its binary on every
//! invocation, since the pipeline enumerates before it runs. That is one extra
//! execution of the test binary per test.

pub mod cli;
mod convert;
pub mod errors;
mod list;
mod output;
mod pipeline;
mod project_root;
mod run_one;

pub use cli::App;
