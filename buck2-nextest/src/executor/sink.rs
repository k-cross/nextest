// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Streaming test results back to Buck2 as they happen.
//!
//! Nextest hands results to a callback that runs on one of its own runtime's
//! worker threads. That callback must not block -- while it is running, no test
//! start, completion, signal, or timeout is processed -- and it cannot block on
//! a future, since blocking inside a runtime panics. Sending a gRPC request
//! from it directly is therefore not an option.
//!
//! So the callback does the cheapest thing that preserves the information: it
//! converts the borrowed event into an owned message and pushes it onto a
//! bounded channel. A dedicated thread drains that channel and makes the calls.
//! This mirrors `nextest-runner`'s own `RecordReporter`, which solves the same
//! problem for on-disk recordings.
//!
//! The channel is bounded, so a Buck2 that stops reading applies backpressure
//! rather than letting results pile up without limit. That backpressure is
//! itself bounded: see `ResultSink::wait_to_send` for why the callback gives up
//! on a Buck2 that never catches up rather than waiting on it forever.

use crate::{
    errors::ExpectedError,
    proto::{
        ConfiguredTargetHandle, ReportTestResultRequest, TestResult, TestStatus,
        test_orchestrator_client::TestOrchestratorClient, test_result::OptionalMsg,
    },
};
use nextest_metadata::RustBinaryId;
use nextest_runner::{
    helpers::plural,
    reporter::events::{ExecutionResultDescription, ReporterEvent, TestEventKind},
};
use std::{
    collections::HashMap,
    sync::mpsc::{self, SyncSender, TrySendError},
    thread,
    time::{Duration, Instant},
};
use tokio::runtime::Handle;
use tonic::transport::Channel;

/// How many results may be in flight before the callback starts waiting.
///
/// Large enough that a normal run never blocks on it, small enough that a
/// wedged Buck2 is noticed rather than absorbed.
const CHANNEL_DEPTH: usize = 128;

/// How long the callback waits for Buck2 to catch up before giving up on it.
///
/// Generous, since exceeding it abandons a run that might still be healthy, and
/// only a Buck2 that has stopped reading results altogether gets this far.
const SEND_TIMEOUT: Duration = Duration::from_secs(60);

/// How often to look for room while the channel is full.
const SEND_RETRY_INTERVAL: Duration = Duration::from_millis(10);

/// One finished test, owned so it can outlive the borrowed event it came from.
#[derive(Clone, Debug)]
struct Finished {
    handle: ConfiguredTargetHandle,
    name: String,
    status: TestStatus,
    duration: Option<Duration>,
    details: String,
    message: Option<String>,
}

/// Reports results to Buck2 from a thread of its own.
#[derive(Debug)]
pub(super) struct ResultSink {
    sender: SyncSender<Finished>,
    handles: HashMap<RustBinaryId, ConfiguredTargetHandle>,
    worker: thread::JoinHandle<Result<(), ExpectedError>>,
}

impl ResultSink {
    /// Starts the reporting thread.
    ///
    /// `runtime` drives the gRPC calls. It must handle to a runtime with worker
    /// threads of its own, since the reporting thread blocks on each call
    /// rather than driving the runtime itself.
    pub(super) fn new(
        client: TestOrchestratorClient<Channel>,
        runtime: Handle,
        handles: HashMap<RustBinaryId, ConfiguredTargetHandle>,
    ) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Finished>(CHANNEL_DEPTH);
        let worker = thread::Builder::new()
            .name("buck2-nextest-results".to_owned())
            .spawn(move || {
                let mut client = client;
                while let Ok(finished) = receiver.recv() {
                    let request = ReportTestResultRequest {
                        result: Some(TestResult {
                            name: finished.name,
                            status: finished.status as i32,
                            msg: finished.message.map(|msg| OptionalMsg { msg }),
                            target: Some(finished.handle),
                            duration: finished.duration.map(duration_to_proto),
                            details: finished.details,
                            max_memory_used_bytes: None,
                        }),
                    };
                    runtime
                        .block_on(client.report_test_result(request))
                        .map_err(|status| ExpectedError::ReportResultsError {
                            status: Box::new(status),
                        })?;
                }
                Ok(())
            })
            .expect("spawning a thread with a valid name succeeds");

        Self {
            sender,
            handles,
            worker,
        }
    }

    /// Forwards an event, if it is one Buck2 cares about.
    ///
    /// Returns an error only when the reporting thread has stopped, which
    /// nextest turns into a graceful cancellation of the run.
    pub(super) fn write_event(&self, event: &ReporterEvent<'_>) -> Result<(), SinkDisconnected> {
        let ReporterEvent::Test(event) = event else {
            return Ok(());
        };

        let finished = match &event.kind {
            TestEventKind::TestFinished {
                test_instance,
                run_statuses,
                ..
            } => {
                let last = run_statuses.last_status();
                let attempts = run_statuses.len();
                Finished {
                    handle: self.handle_for(test_instance.binary_id),
                    name: test_instance.test_name.to_string(),
                    status: status_for(&last.result),
                    duration: Some(last.time_taken),
                    details: last
                        .error_summary
                        .as_ref()
                        .map_or_else(String::new, |summary| summary.description.clone()),
                    // Buck2 only sees the final status, so a test that needed
                    // more than one attempt says so.
                    message: (attempts > 1).then(|| {
                        format!("{attempts} {} were made", plural::attempts_str(attempts))
                    }),
                }
            }
            TestEventKind::TestSkipped { test_instance, .. } => Finished {
                handle: self.handle_for(test_instance.binary_id),
                name: test_instance.test_name.to_string(),
                status: TestStatus::Skip,
                duration: None,
                details: String::new(),
                message: None,
            },
            _ => return Ok(()),
        };

        match self.sender.try_send(finished) {
            Ok(()) => Ok(()),
            // A full channel usually means Buck2 is slow rather than gone, so
            // it is worth waiting for -- but only so long. See `wait_to_send`.
            Err(TrySendError::Full(finished)) => {
                wait_to_send(&self.sender, finished, SEND_TIMEOUT, SEND_RETRY_INTERVAL)
            }
            Err(TrySendError::Disconnected(_)) => Err(SinkDisconnected),
        }
    }

    /// Stops the reporting thread and waits for the backlog to drain.
    pub(super) fn finish(self) -> Result<(), ExpectedError> {
        let Self { sender, worker, .. } = self;
        // Dropping the sender is what ends the receive loop.
        drop(sender);
        worker
            .join()
            .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
    }

    fn handle_for(&self, binary_id: &RustBinaryId) -> ConfiguredTargetHandle {
        // Every binary in the list came from a spec, so a miss would mean the
        // two got out of step: a bug here, not a runtime condition.
        *self
            .handles
            .get(binary_id)
            .unwrap_or_else(|| panic!("no Buck2 target handle for binary `{binary_id}`"))
    }
}

/// The reporting thread is gone, so no further results can be delivered.
#[derive(Clone, Copy, Debug)]
pub(super) struct SinkDisconnected;

/// Waits a bounded time for room in a full channel.
///
/// This runs on one of the runner's threads, so waiting here stops the run from
/// making progress: no test starts or finishes, no timeout fires, and `Ctrl-C`
/// does not reach the cancellation path. Waiting a little keeps a slow Buck2
/// from costing results. Waiting forever would leave the run wedged with
/// nothing able to end it, so a Buck2 that never catches up is reported as gone
/// instead, which cancels the run gracefully.
fn wait_to_send<T>(
    sender: &SyncSender<T>,
    value: T,
    timeout: Duration,
    interval: Duration,
) -> Result<(), SinkDisconnected> {
    let deadline = Instant::now() + timeout;
    let mut value = value;
    loop {
        thread::sleep(interval);
        match sender.try_send(value) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Full(returned)) => {
                if Instant::now() >= deadline {
                    return Err(SinkDisconnected);
                }
                value = returned;
            }
            Err(TrySendError::Disconnected(_)) => return Err(SinkDisconnected),
        }
    }
}

/// Maps nextest's outcome onto the protocol's.
///
/// Buck2's vocabulary is coarser than nextest's: it has no way to say "leaked a
/// handle" or "passed on the second attempt". Anything nextest counts as a
/// success is reported as a pass, so Buck2's summary agrees with nextest's.
fn status_for(result: &ExecutionResultDescription) -> TestStatus {
    if result.is_success() {
        return TestStatus::Pass;
    }
    match result {
        ExecutionResultDescription::Timeout { .. } => TestStatus::Timeout,
        // A test that could not be executed at all is an infrastructure
        // problem rather than a test that failed on its own terms.
        ExecutionResultDescription::ExecFail => TestStatus::Fatal,
        _ => TestStatus::Fail,
    }
}

fn duration_to_proto(duration: Duration) -> prost_types::Duration {
    // A wall-clock test duration cannot realistically overflow an i64 of
    // seconds; saturate rather than panic if one somehow does.
    prost_types::Duration {
        seconds: i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
        nanos: i32::try_from(duration.subsec_nanos()).unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nextest_runner::{
        config::elements::{LeakTimeoutResult, SlowTimeoutResult},
        reporter::events::FailureDescription,
    };

    #[test]
    fn nextest_outcomes_map_onto_buck2_statuses() {
        assert_eq!(
            status_for(&ExecutionResultDescription::Pass),
            TestStatus::Pass
        );
        assert_eq!(
            status_for(&ExecutionResultDescription::Leak {
                result: LeakTimeoutResult::Pass
            }),
            TestStatus::Pass,
            "a leak that nextest forgives is still a pass to Buck2"
        );
        assert_eq!(
            status_for(&ExecutionResultDescription::Leak {
                result: LeakTimeoutResult::Fail
            }),
            TestStatus::Fail,
        );
        assert_eq!(
            status_for(&ExecutionResultDescription::Timeout {
                result: SlowTimeoutResult::Fail
            }),
            TestStatus::Timeout,
        );
        assert_eq!(
            status_for(&ExecutionResultDescription::Timeout {
                result: SlowTimeoutResult::Pass
            }),
            TestStatus::Pass,
            "a timeout configured to pass is a pass, not a timeout failure"
        );
        assert_eq!(
            status_for(&ExecutionResultDescription::ExecFail),
            TestStatus::Fatal,
        );
        assert_eq!(
            status_for(&ExecutionResultDescription::Fail {
                failure: FailureDescription::ExitCode { code: 101 },
                leaked: false,
            }),
            TestStatus::Fail,
        );
    }

    #[test]
    fn durations_survive_conversion() {
        let converted = duration_to_proto(Duration::new(3, 500_000_000));
        assert_eq!(converted.seconds, 3);
        assert_eq!(converted.nanos, 500_000_000);
    }

    /// A slow Buck2 costs a wait, not results: room appearing before the
    /// deadline is enough.
    #[test]
    fn a_channel_that_drains_in_time_accepts_the_value() {
        let (sender, receiver) = mpsc::sync_channel::<u32>(1);
        sender.try_send(1).expect("the channel starts empty");

        let drainer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            // Holding the receiver keeps the channel connected; taking one
            // value is what makes room.
            let taken = receiver.recv().expect("a value is waiting");
            (taken, receiver)
        });

        wait_to_send(
            &sender,
            2,
            Duration::from_secs(30),
            Duration::from_millis(5),
        )
        .expect("room appeared well before the deadline");

        let (taken, receiver) = drainer.join().expect("the draining thread finishes");
        assert_eq!(taken, 1);
        assert_eq!(receiver.recv().expect("the waited-for value arrived"), 2);
    }

    /// A Buck2 that never reads again must not wedge the run: the wait ends,
    /// and ending it is reported as a disconnect so the run is cancelled.
    #[test]
    fn a_channel_that_never_drains_gives_up() {
        let (sender, _receiver) = mpsc::sync_channel::<u32>(1);
        sender.try_send(1).expect("the channel starts empty");

        let started = Instant::now();
        wait_to_send(
            &sender,
            2,
            Duration::from_millis(50),
            Duration::from_millis(5),
        )
        .expect_err("the deadline passes with the channel still full");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the wait is bounded, took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_dropped_receiver_is_reported_at_once() {
        let (sender, receiver) = mpsc::sync_channel::<u32>(1);
        sender.try_send(1).expect("the channel starts empty");
        drop(receiver);

        // A timeout long enough that returning promptly can only mean the
        // disconnect was noticed rather than waited out.
        let started = Instant::now();
        wait_to_send(
            &sender,
            2,
            Duration::from_secs(60),
            Duration::from_millis(5),
        )
        .expect_err("a dropped receiver cannot take the value");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the disconnect ended the wait, took {:?}",
            started.elapsed()
        );
    }
}
