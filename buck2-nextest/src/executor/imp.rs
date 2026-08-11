// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Running a Buck2 test session.
//!
//! ## Threads
//!
//! Three thread contexts are in play, and which code runs where is load-bearing
//! rather than incidental:
//!
//! * A tokio runtime owns both gRPC connections. It has worker threads of its
//!   own, so the connections keep making progress while the main thread is
//!   busy running tests.
//! * The main thread drives the run. It calls `block_on` for each phase that
//!   talks to Buck2, but is outside the runtime by the time tests start --
//!   which it must be, since [`TestRunner::try_execute`] builds a runtime of its
//!   own and would panic if nested inside one.
//! * A reporting thread, owned by [`ResultSink`], drains results and sends them.
//!   See that module for why the callback cannot send them itself.
//!
//! ## Phases
//!
//! 1. Connect to the orchestrator, and serve the executor service.
//! 2. Collect specs until Buck2 says there are no more.
//! 3. Ask Buck2 how to run each one.
//! 4. Run them, streaming results back as they finish.
//! 5. Report the exit code and stop.
//!
//! [`TestRunner::try_execute`]: nextest_runner::runner::TestRunner::try_execute

use super::{
    prepare::{PreparedTarget, prepare_all},
    service::SpecCollector,
    sink::ResultSink,
    transport::{Socket, SocketSpec, serve_test_executor},
};
use crate::{
    cli::{FilterOpts, absolute_project_root},
    convert::to_binary_list,
    errors::{ExpectedError, Result},
    proto::{
        ConfiguredTargetHandle, EndOfTestResultsRequest, test_executor_server::TestExecutorServer,
        test_orchestrator_client::TestOrchestratorClient,
    },
    run::RunContext,
    spec::Buck2TestTarget,
};
use camino::{Utf8Path, Utf8PathBuf};
use nextest_metadata::{NextestExitCode, RustBinaryId};
use nextest_runner::{helpers::force_or_new_run_id, platform::BuildPlatforms};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};
use tokio::{runtime::Runtime, sync::oneshot, task::JoinHandle};
use tonic::transport::Channel;

/// Everything the executor needs that did not come from Buck2.
#[derive(Debug)]
pub struct ExecutorOptions {
    /// The Buck2 project root, if the command line named one, made absolute.
    ///
    /// Buck2 does not name it, so this is normally `None` and the root is taken
    /// from what Buck2 says about the targets it sends.
    pub project_root: Option<Utf8PathBuf>,

    /// The nextest profile to use.
    pub profile_name: Option<String>,

    /// A path to a nextest configuration file, if one was given.
    pub config_file: Option<Utf8PathBuf>,

    /// Filters from the arguments after `--`.
    pub filter: FilterOpts,

    /// Environment variables to add to every test process.
    ///
    /// These come from `buck2 test ... -- --env NAME=VALUE`. They are applied
    /// on top of what Buck2 says each target needs, so a variable named in both
    /// places takes the value given here.
    pub extra_env: BTreeMap<String, String>,

    /// The full command line, recorded with the run.
    pub cli_args: Vec<String>,
}

/// Runs a session against Buck2, returning the process exit code.
pub fn exec(
    executor_socket: SocketSpec,
    orchestrator_socket: SocketSpec,
    options: ExecutorOptions,
) -> Result<i32> {
    // Two workers: one is enough to drive the connections, and a second keeps a
    // slow call from stalling the other socket.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .thread_name("buck2-nextest-grpc")
        .build()
        .map_err(|error| ExpectedError::RuntimeCreateError { error })?;

    let session = runtime.block_on(collect(executor_socket, orchestrator_socket))?;
    let prepared = runtime.block_on(prepare(session))?;

    // From here on the main thread is outside the runtime, which is what lets
    // the runner build its own.
    run(&runtime, prepared, options)
}

/// The pieces phase 1 produces.
struct Session {
    client: TestOrchestratorClient<Channel>,
    specs: Vec<crate::proto::ExternalRunnerSpec>,
    /// Held so the executor service keeps serving; dropping it shuts it down.
    _shutdown: oneshot::Sender<()>,
}

async fn collect(executor_socket: SocketSpec, orchestrator_socket: SocketSpec) -> Result<Session> {
    let executor_socket = Socket::adopt(executor_socket, "executor").await?;
    let orchestrator_socket = Socket::adopt(orchestrator_socket, "orchestrator").await?;

    let channel = orchestrator_socket.into_channel("orchestrator").await?;
    let client = TestOrchestratorClient::new(channel);

    let (specs, shutdown) = collect_specs(executor_socket).await?;

    Ok(Session {
        client,
        specs,
        _shutdown: shutdown,
    })
}

/// Serves the executor service until Buck2 says it has sent every spec.
///
/// Returns the specs along with the token that keeps the service alive: Buck2
/// may call back on that connection later, so it is shut down only once the run
/// is over.
async fn collect_specs(
    executor_socket: Socket,
) -> Result<(Vec<crate::proto::ExternalRunnerSpec>, oneshot::Sender<()>)> {
    let (collector, specs_rx) = SpecCollector::new();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let (hangup_tx, hangup_rx) = oneshot::channel();
    let service = TestExecutorServer::new(collector);

    // The service runs as a task so that this function can await the specs it
    // produces. Serving ends when `shutdown_tx` drops.
    let serving = tokio::spawn(serve_test_executor(
        executor_socket,
        service,
        hangup_tx,
        async {
            let _ = shutdown_rx.await;
        },
    ));

    // Two things can go wrong, and neither shows up as the other. Serving can
    // fail outright, which drops the collector and with it the sender; or the
    // connection can close without `EndOfTestRequests`, which tonic reports to
    // nobody -- hence the hangup token.
    //
    // `biased` so that a Buck2 which sends the specs and disconnects
    // immediately afterwards is read as the completed handshake it is: both
    // arms are then ready, and only the specs are the answer.
    let specs = tokio::select! {
        biased;
        result = specs_rx => match result {
            Ok(specs) => specs,
            Err(_) => return Err(serve_error(serving).await),
        },
        _ = hangup_rx => {
            // Let serving finish so its own error, if it had one, is the one
            // reported.
            drop(shutdown_tx);
            return Err(serve_error(serving).await);
        }
    };

    Ok((specs, shutdown_tx))
}

/// Waits for serving to end, and says why the session did.
///
/// Serving that ended without an error of its own means Buck2 hung up.
async fn serve_error(serving: JoinHandle<Result<()>>) -> ExpectedError {
    match serving.await {
        Ok(Err(error)) => error,
        Ok(Ok(())) | Err(_) => ExpectedError::Buck2Disconnected,
    }
}

/// The pieces phase 3 produces.
struct Prepared {
    client: TestOrchestratorClient<Channel>,
    targets: PreparedTargets,
    _shutdown: oneshot::Sender<()>,
}

/// What Buck2's answers amount to, once checked over.
#[derive(Debug)]
struct PreparedTargets {
    targets: Vec<Buck2TestTarget>,
    handles: HashMap<RustBinaryId, ConfiguredTargetHandle>,

    /// The project root the answers imply, if they implied one.
    ///
    /// Buck2 does not pass the root to the executor; see `project_root_from` in
    /// the `prepare` module for how it is worked out and why.
    project_root: Option<Utf8PathBuf>,
}

async fn prepare(session: Session) -> Result<Prepared> {
    let Session {
        mut client,
        specs,
        _shutdown,
    } = session;

    let prepared = prepare_all(&mut client, specs).await?;

    Ok(Prepared {
        client,
        targets: check_prepared(prepared)?,
        _shutdown,
    })
}

/// Sorts prepared targets into what the run needs, rejecting what it cannot use.
///
/// The binary ID is derived from the label, exactly as [`to_binary_list`]
/// derives it, so a result can be matched back to the target it came from.
fn check_prepared(prepared: Vec<PreparedTarget>) -> Result<PreparedTargets> {
    let mut targets = Vec::with_capacity(prepared.len());
    let mut handles = HashMap::with_capacity(prepared.len());
    let mut project_root: Option<Utf8PathBuf> = None;

    for item in prepared {
        // A label Buck2 has already sent means two configured targets share one
        // binary ID -- the label leaves the configuration out, so a target
        // reached under two of them arrives twice. Nothing downstream can tell
        // the two apart: the binary list would keep one, and the other's tests
        // would silently never run while Buck2 waited for results that never
        // came. Say so instead.
        if handles
            .insert(RustBinaryId::new(&item.target.label), item.handle)
            .is_some()
        {
            return Err(ExpectedError::DuplicateTargetLabel {
                label: item.target.label,
            });
        }

        // One session is one project, so every target must agree on where its
        // root is. A disagreement means the root was derived wrongly for at
        // least one of them, and quietly picking either would put every path
        // nextest reports somewhere it is not.
        if let Some(implied) = item.project_root {
            match &project_root {
                None => project_root = Some(implied),
                Some(root) if root == &implied => {}
                Some(root) => {
                    return Err(ExpectedError::ConflictingProjectRoots {
                        first: root.clone(),
                        second: implied,
                        label: item.target.label,
                    });
                }
            }
        }

        targets.push(item.target);
    }

    Ok(PreparedTargets {
        targets,
        handles,
        project_root,
    })
}

fn run(runtime: &Runtime, prepared: Prepared, options: ExecutorOptions) -> Result<i32> {
    let Prepared {
        client,
        targets:
            PreparedTargets {
                mut targets,
                handles,
                project_root,
            },
        _shutdown,
    } = prepared;

    for target in &mut targets {
        target.env.extend(
            options
                .extra_env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
    }

    // What Buck2 implied, unless the command line said otherwise. With no
    // targets at all there is nothing to imply it and nothing that depends on
    // it, so the process's own directory stands in.
    let project_root = match options.project_root.or(project_root) {
        Some(root) => root,
        None => absolute_project_root(Utf8Path::new("."))?,
    };

    let build_platforms = BuildPlatforms::new_with_no_target().map_err(|error| {
        ExpectedError::HostPlatformDetectError {
            error: Box::new(error),
        }
    })?;
    let binaries = to_binary_list(&targets, &project_root, build_platforms);

    let cx = RunContext {
        binaries,
        project_root,
        profile_name: options.profile_name,
        config_file: options.config_file,
        run_id: force_or_new_run_id(),
        filtersets: options.filter.filtersets(),
        filter_patterns: options.filter.patterns(),
        run_ignored: options.filter.run_ignored(),
        filter_bound: options.filter.filter_bound(),
        list_threads: options.filter.list_threads(),
    };

    let sink = Arc::new(ResultSink::new(
        client.clone(),
        runtime.handle().clone(),
        handles,
    ));

    // Buck2 renders results itself, so nextest's own reporter is silenced;
    // `--test-executor-stderr=-` on the Buck2 side is how a person sees it.
    let exit_code = cx.run_with_sink(options.cli_args, {
        let sink = Arc::clone(&sink);
        move |event| sink.write_event(event)
    });

    // Drain results before reporting the exit code, so Buck2 has every result
    // in hand when it is told the run is over.
    let sink = Arc::into_inner(sink).expect("the run has finished, so no callback holds the sink");
    let drained = sink.finish();

    // A failure to deliver results comes first. When reporting breaks, the run
    // is cancelled by the very error that broke it, so `exit_code` carries only
    // the write failure that followed -- which says nothing about the cause.
    drained?;

    // `cargo-nextest` treats a run with no tests in it as an error, on the
    // grounds that the person asked for tests and got none. Buck2 chooses the
    // target set itself, so `buck2 test //:some-library` finding no tests is
    // routine rather than a mistake -- and Buck2 says "0 tests" plainly either
    // way.
    let exit_code = match exit_code? {
        NextestExitCode::NO_TESTS_RUN => 0,
        code => code,
    };

    let mut client = client;
    runtime
        .block_on(client.end_of_test_results(EndOfTestResultsRequest { exit_code }))
        .map_err(|status| ExpectedError::ReportResultsError {
            status: Box::new(status),
        })?;

    Ok(exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        proto::{
            ConfiguredTarget as ProtoConfiguredTarget, Empty,
            ExternalRunnerSpec as ProtoExternalRunnerSpec, ExternalRunnerSpecRequest,
            ExternalRunnerSpecValue, external_runner_spec_value,
            test_executor_client::TestExecutorClient,
        },
        spec::ConfiguredTarget,
    };
    use std::{collections::BTreeMap, time::Duration};
    use tokio::net::{TcpListener, TcpStream};

    /// How long a test waits before deciding a future is never going to finish.
    ///
    /// Generous: everything here is local and takes microseconds, so this only
    /// bounds a hang. A failure by timeout is the point of two of these tests.
    const TIMEOUT: Duration = Duration::from_secs(30);

    /// Two ends of a connected socket, standing in for what Buck2 hands over.
    ///
    /// TCP rather than a Unix socket pair so that these run on every platform,
    /// as they do when Buck2 is not on Unix.
    async fn socket_pair() -> (Socket, Socket) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port is available");
        let addr = listener.local_addr().expect("the listener has an address");
        let (accepted, connected) = tokio::join!(listener.accept(), TcpStream::connect(addr));
        let (accepted, _) = accepted.expect("the connection is accepted");
        (
            Socket::Tcp(accepted),
            Socket::Tcp(connected.expect("the connection is established")),
        )
    }

    fn spec_request(target: &str) -> ExternalRunnerSpecRequest {
        ExternalRunnerSpecRequest {
            test_spec: Some(ProtoExternalRunnerSpec {
                target: Some(ProtoConfiguredTarget {
                    cell: "root".to_owned(),
                    package: String::new(),
                    target: target.to_owned(),
                    ..ProtoConfiguredTarget::default()
                }),
                test_type: "rust".to_owned(),
                command: vec![ExternalRunnerSpecValue {
                    value: Some(external_runner_spec_value::Value::Verbatim(
                        "/fake/binary".to_owned(),
                    )),
                }],
                ..ProtoExternalRunnerSpec::default()
            }),
        }
    }

    async fn with_timeout<T>(future: impl Future<Output = T>) -> T {
        tokio::time::timeout(TIMEOUT, future)
            .await
            .expect("the future finishes rather than waiting forever")
    }

    #[tokio::test]
    async fn specs_arrive_over_the_socket() {
        let (ours, theirs) = socket_pair().await;
        let collecting = tokio::spawn(collect_specs(ours));

        let channel = theirs
            .into_channel("test")
            .await
            .expect("a channel is built over the other end");
        let mut client = TestExecutorClient::new(channel);
        client
            .external_runner_spec(spec_request("demo"))
            .await
            .expect("the spec is accepted");
        client
            .end_of_test_requests(Empty {})
            .await
            .expect("the end of requests is accepted");

        let (specs, _shutdown) = with_timeout(collecting)
            .await
            .expect("the task did not panic")
            .expect("the specs are collected");
        assert_eq!(specs.len(), 1);
        assert_eq!(
            specs[0]
                .target
                .as_ref()
                .expect("the target is set")
                .target
                .as_str(),
            "demo"
        );
    }

    /// Buck2 may close the connection as soon as it has been told that the
    /// specs were received. That is a completed handshake, not a hangup.
    #[tokio::test]
    async fn a_disconnect_after_the_last_spec_is_not_an_error() {
        let (ours, theirs) = socket_pair().await;
        let collecting = tokio::spawn(collect_specs(ours));

        let channel = theirs
            .into_channel("test")
            .await
            .expect("a channel is built over the other end");
        let mut client = TestExecutorClient::new(channel);
        client
            .external_runner_spec(spec_request("demo"))
            .await
            .expect("the spec is accepted");
        client
            .end_of_test_requests(Empty {})
            .await
            .expect("the end of requests is accepted");
        drop(client);

        let (specs, _shutdown) = with_timeout(collecting)
            .await
            .expect("the task did not panic")
            .expect("the specs are collected despite the disconnect");
        assert_eq!(specs.len(), 1);
    }

    /// The case that has nothing to time it out: Buck2 goes away without ever
    /// saying it was done, so the specs it would have sent never arrive.
    #[tokio::test]
    async fn a_disconnect_before_the_last_spec_is_an_error() {
        let (ours, theirs) = socket_pair().await;
        let collecting = tokio::spawn(collect_specs(ours));

        // Never speaks gRPC at all: the connection simply goes.
        drop(theirs);

        let error = with_timeout(collecting)
            .await
            .expect("the task did not panic")
            .expect_err("a disconnect before the end of requests is an error");
        assert!(
            matches!(error, ExpectedError::Buck2Disconnected),
            "unexpected error: {error:?}"
        );
    }

    /// Same, but after some specs have arrived: a partial set is not a set.
    #[tokio::test]
    async fn a_disconnect_partway_through_is_an_error() {
        let (ours, theirs) = socket_pair().await;
        let collecting = tokio::spawn(collect_specs(ours));

        let channel = theirs
            .into_channel("test")
            .await
            .expect("a channel is built over the other end");
        let mut client = TestExecutorClient::new(channel);
        client
            .external_runner_spec(spec_request("demo"))
            .await
            .expect("the spec is accepted");
        drop(client);

        let error = with_timeout(collecting)
            .await
            .expect("the task did not panic")
            .expect_err("a disconnect before the end of requests is an error");
        assert!(
            matches!(error, ExpectedError::Buck2Disconnected),
            "unexpected error: {error:?}"
        );
    }

    fn prepared_target(
        target: &str,
        configuration: &str,
        handle: i64,
        project_root: Option<&str>,
    ) -> PreparedTarget {
        let configured = ConfiguredTarget {
            cell: "root".to_owned(),
            package: String::new(),
            target: target.to_owned(),
            configuration: Some(configuration.to_owned()),
        };
        PreparedTarget {
            target: Buck2TestTarget {
                label: configured.label(),
                target: configured,
                program: "/fake/binary".to_owned(),
                leading_args: Vec::new(),
                env: BTreeMap::new(),
                cwd: project_root.map(Into::into),
            },
            handle: ConfiguredTargetHandle { id: handle },
            project_root: project_root.map(Into::into),
        }
    }

    #[test]
    fn prepared_targets_are_sorted_into_targets_and_handles() {
        let checked = check_prepared(vec![
            prepared_target("one", "cfg:linux", 1, Some("/project")),
            prepared_target("two", "cfg:linux", 2, Some("/project")),
        ])
        .expect("distinct labels are accepted");

        let labels: Vec<_> = checked
            .targets
            .iter()
            .map(|target| target.label.as_str())
            .collect();
        assert_eq!(labels, ["root//:one", "root//:two"]);
        assert_eq!(
            checked.handles.get(&RustBinaryId::new("root//:one")),
            Some(&ConfiguredTargetHandle { id: 1 })
        );
        assert_eq!(
            checked.handles.get(&RustBinaryId::new("root//:two")),
            Some(&ConfiguredTargetHandle { id: 2 })
        );
        assert_eq!(
            checked.project_root.as_deref(),
            Some(Utf8Path::new("/project"))
        );
    }

    /// The same target under two configurations has one label, so one of the two
    /// would otherwise be dropped without a word.
    #[test]
    fn two_configurations_of_one_target_are_rejected() {
        let error = check_prepared(vec![
            prepared_target("demo", "cfg:linux", 1, Some("/project")),
            prepared_target("demo", "cfg:macos", 2, Some("/project")),
        ])
        .expect_err("one label cannot stand for two targets");
        assert!(
            matches!(&error, ExpectedError::DuplicateTargetLabel { label } if label == "root//:demo"),
            "unexpected error: {error:?}"
        );
    }

    /// A target that says nothing about where it runs neither sets the project
    /// root nor disagrees with one.
    #[test]
    fn a_target_without_a_root_leaves_the_others_to_it() {
        let checked = check_prepared(vec![
            prepared_target("one", "cfg:linux", 1, None),
            prepared_target("two", "cfg:linux", 2, Some("/project")),
            prepared_target("three", "cfg:linux", 3, None),
        ])
        .expect("targets without a root are accepted");
        assert_eq!(
            checked.project_root.as_deref(),
            Some(Utf8Path::new("/project"))
        );

        let checked = check_prepared(vec![prepared_target("one", "cfg:linux", 1, None)])
            .expect("a target without a root is accepted");
        assert_eq!(checked.project_root, None);
    }

    #[test]
    fn two_project_roots_are_rejected() {
        let error = check_prepared(vec![
            prepared_target("one", "cfg:linux", 1, Some("/project")),
            prepared_target("two", "cfg:linux", 2, Some("/elsewhere")),
        ])
        .expect_err("one session cannot span two projects");
        assert!(
            matches!(
                &error,
                ExpectedError::ConflictingProjectRoots { first, second, label }
                    if first == "/project" && second == "/elsewhere" && label == "root//:two"
            ),
            "unexpected error: {error:?}"
        );
    }
}
