---
icon: material/connection
description: Design document describing the contract between build systems and nextest, and the nextest-session crate that carries it.
---

# Build system integration

!!! abstract "Design document"

    This is a design document intended for nextest contributors and curious readers.

Nextest's runner does not care who built the test binaries. The
`nextest-session` crate is the contract between a build system and nextest: a
frontend supplies what it built, and the crate carries it through the shared
pipeline — configuration, profiles, filtersets, listing, running, reporting,
and the exit-code policy. `cargo-nextest` (Cargo) and `buck2-nextest` (Buck2)
are both frontends of this crate, so the contract is exercised by nextest's
richest consumer and its simplest one alike. A new build system integration
starts here.

## Why data, not a trait

The variation between build systems is in how test binaries are *discovered
and described*: Cargo streams compiler messages from `cargo test --no-run`,
Buck2 names one test binary per invocation on the command line, and a future
integration might read a manifest. Once the binaries are described, nothing about the pipeline
varies per build system, and there is no point mid-run where nextest needs to
call back into build-system-specific behavior — reporting is already a
callback, and results flow out through it.

So the contract's currency is plain data. An integration constructs values and
hands them over; it does not implement an interface for nextest to call.

## What a build system supplies

* `RustTestBinary`, one per test binary, collected into a `BinaryList` along
  with a `RustBuildMeta` describing the build. Binary IDs are the integration's
  own vocabulary — Buck2 uses target labels, so `binary_id()` filtersets and
  reporter output speak in labels.
* `PackageInfo`, one per package named by a binary's `package_id`. A build
  system without Cargo's package vocabulary synthesizes these: `buck2-nextest`
  derives them from Buck2 labels, with the `BUCK` file standing in for the
  manifest and a `0.0.0` version.
* `TestBinaryInvocation`, per binary, for launchers that need extra leading
  arguments, environment variables, or a working directory. Cargo-built
  binaries need none of this; Buck2-built binaries often do. This is
  deliberately *not* serialized into binary-list metadata, so archives do not
  round-trip it.
* A workspace root — tests see it as an absolute `NEXTEST_WORKSPACE_ROOT`, and
  configuration is read from `.config/nextest.toml` under it — and
  `BuildPlatforms` for the host and target.

## What the pipeline provides

In order:

1. `NextestConfig::from_sources` loads configuration, taking a `ParseContext`.
   `ParseContext::without_graph` exists for integrations with no Cargo package
   graph; it disables the package-graph filterset predicates (`package()`,
   `deps()`, `rdeps()`) and nothing else.
2. `evaluate_profile` turns an `EarlyProfile` into an `EvaluatableProfile`,
   creating the profile's store directory if it writes a JUnit report.
3. `parse_filtersets` compiles filterset inputs against the profile's known
   test groups, reporting every bad one at once.
4. `TestSession::build` executes the binaries to enumerate their tests,
   producing a `TestList` to write out (for listing) or run.
5. `TestSession::build_runner` and `run_to_completion` execute the tests,
   feeding every event to the frontend's reporter — so per-test process
   isolation, retries, timeouts, and leak detection work identically under
   every build system.
6. `final_outcome` maps the finished run to the canonical exit-code policy.

## The sink, and who owns the terminal

`run_to_completion` takes a *sink*: a callback that sees every reporter event
before the reporter renders it. This is how a build system that renders
results itself consumes them — `buck2-nextest` collects the one test's outcome
from the events and writes it as the JSON Buck2 parses, while nextest's own
reporter writes plainly to standard error as a detail view. If the sink returns an error, the run is cancelled gracefully:
nextest keeps reporting until the tests it has already started finish. A
frontend with no sink passes an infallible closure and recovers the reporter's
own error type with `into_report_errors`.

Reporter construction stays with the frontend, and this is a hard rule rather
than a convenience: `ReporterOutput::Writer` borrows its writer and is
invariant over that lifetime, so the output must be built in the scope that
owns the writer. A shared function that built the reporter internally would
force the writer to be `'static` or fight the borrow checker.

## The exit-code policy

`final_outcome` and `RunFailure` are the shared truth about how a finished run
is judged, so frontends cannot drift apart:

| Outcome | Exit code |
| --- | --- |
| Success | 0 |
| No tests were selected to run | 4 (`NO_TESTS_RUN`) |
| A rerun finished with outstanding tests unseen | 5 (`RERUN_TESTS_OUTSTANDING`) |
| At least one test failed | 100 (`TEST_RUN_FAILED`) |
| A setup script failed | 105 (`SETUP_SCRIPT_FAILED`) |

`NoTestsBehavior` is the one policy knob: a run selecting no tests can pass,
warn, or fail. Nextest's default is to fail, on the grounds that the person
asked for tests and got none.

A frontend that runs one named test at a time may not want this table at all.
`buck2-nextest` reports each test's outcome to Buck2 as JSON and derives its
exit code from that outcome, because `final_outcome` judges a whole run: it
calls a skipped test "no tests were selected", which is the right answer for a
run and the wrong one for a single test Buck2 chose not to run.

Each frontend maps `RunFailure` onto its own error type for rendering; the
codes themselves come from `RunFailure::exit_code` and are the same
everywhere.

## What stays frontend policy

Everything in front of the contract is acquisition, and everything around it
is presentation. Both stay out of `nextest-session`:

* **Acquisition**: Cargo metadata and the package graph, `cargo test --no-run`
  and message parsing, reuse-build archives and path remapping, `.cargo/config.toml`
  environment; the Buck2 target label, binary path, and harness arguments
  `buck2-nextest` is invoked with. Each produces the contract's inputs.
* **Presentation and policy**: CLI parsing, user configuration and its
  precedence rules, reporter display options, pagers, version-requirement
  gates, and double-spawn or target-runner enablement. The contract's currency
  here is finished `TestRunnerBuilder` and `ReporterBuilder` values.
* **Recording and replay** stay in `cargo-nextest` for now. They are not
  intrinsically Cargo-specific, but the recording format currently embeds raw
  `cargo metadata` JSON, and replay reconstructs a package graph from it.
  Making recording available behind the contract means replacing that payload
  with an orchestrator-agnostic package description — a named follow-up, not
  part of this design.

## An executable specification

The `nextest-session` crate-level documentation carries a compile-checked
example that walks a synthetic single-binary integration through the whole
pipeline. When the contract changes, that example fails to build — it is the
specification of record, with `buck2-nextest` as the reference implementation
of a complete frontend.
