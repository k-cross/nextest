---
icon: material/hammer-wrench
---

# Buck2

`buck2-nextest` lets [Buck2](https://buck2.build/) run Rust tests with nextest. Buck2 discovers,
schedules, and caches the tests; nextest executes them, so each test sees the same configuration,
retries, slow-test handling, and leak detection it would under `cargo nextest run`.

!!! warning "Experimental, and blocked on Buck2"

    `buck2-nextest` is not released, and it depends on Buck2's `InternalRunnerTestInfo` provider,
    which does not yet work in open-source Buck2. See [Buck2 support](#buck2-support) below.

## How it works

A target built by the `nextest_test` rule returns Buck2's `InternalRunnerTestInfo` provider instead
of the usual `ExternalRunnerTestInfo`. Buck2 then runs two commands itself, as ordinary build
actions:

1. `buck2-nextest list` once per test target, to find out which tests the binary contains.
2. `buck2-nextest run` once per discovered test, with that test's name appended.

Each command writes JSON that Buck2 parses through the rule's Starlark callbacks. Those callbacks
cannot run anything, so all the judgement lives in `buck2-nextest` and they are left as
`json.decode`.

The division of labour follows from that. Buck2 owns everything spanning tests — scheduling,
concurrency, caching, and the results UI — because it is the one running the actions. Nextest owns
everything within a test, because `run` drives the real nextest pipeline.

The visible difference from a stock `rust_test` is that a test binary is no longer one opaque unit
of work: `buck2 test` shows a row per test, and caches per test.

## Setting it up

Copy the `nextest` cell from `buck2-nextest/buck/nextest/` into your project and declare it in
`.buckconfig`:

```ini
[cells]
  nextest = nextest
```

Declare a toolchain that says where the binary is:

```python
load("@nextest//:nextest_toolchain.bzl", "system_nextest_toolchain")

system_nextest_toolchain(
    name = "nextest",
    visibility = ["PUBLIC"],
)
```

`system_nextest_toolchain` finds `buck2-nextest` on `PATH`. For remote execution, use
`nextest_toolchain` instead and point it at a target producing the binary, so Buck2 materializes it
wherever the action runs:

```python
load("@nextest//:nextest_toolchain.bzl", "nextest_toolchain")

nextest_toolchain(
    name = "nextest",
    nextest = "//tools:buck2-nextest",
    visibility = ["PUBLIC"],
)
```

Then declare test targets with `nextest_test`, which takes exactly the attributes `rust_test` does:

```python
load("@nextest//:nextest_test.bzl", "nextest_test")

nextest_test(
    name = "my-test",
    srcs = ["src/lib.rs"],
    crate_root = "src/lib.rs",
    edition = "2024",
)
```

## Running tests

```console
$ buck2 test //...
```

Buck2's own selection works as usual, including label filtering, since the rule passes labels and
contacts through:

```console
$ buck2 test //... --exclude slow
```

## Configuration

Configuration comes from `.config/nextest.toml` at the Buck2 project root, exactly as it comes from
the workspace root under Cargo. See [Configuration](../configuration/index.md).

Buck2 builds the command line, so a profile is chosen on the toolchain rather than per run:

```python
system_nextest_toolchain(
    name = "nextest",
    profile = "ci",
    config_file = "//:nextest-config",
    visibility = ["PUBLIC"],
)
```

`config_file` is a source, so naming it makes the configuration an input to every test action —
which is what lets it be found under remote execution.

A profile's [default filter](../selecting.md#running-a-subset-of-tests-by-default) applies to
*listing*, so Buck2 never schedules an action for a test the filter would discard. It is not applied
again when running: Buck2 chose that test from what it was told, and is waiting for a result about
it.

Because [filtersets](../filtersets/index.md) here have no Cargo package graph to resolve against,
`package()`, `deps()`, and `rdeps()` are unavailable. Binary IDs are Buck2 labels, so a filterset
reads `binary_id(cell//path/to:target)`.

Tests run in the directory Buck2 ran the action in, which is the project root. Nextest reports that
same directory as `CARGO_MANIFEST_DIR`, and the project root as an absolute
`NEXTEST_WORKSPACE_ROOT`. This is what makes the project-relative paths Buck2 hands a test through
the environment — `$(location ...)` and friends — resolve.

A target's `env` is applied to the test process, in both the listing and run phases. A
[wrapper script](../configuration/wrapper-scripts.md)'s environment overrides it, as do the
[variables nextest sets](../configuration/env-vars.md) for every test. The rule passes it to
`buck2-nextest` as data rather than setting it on the action, so a target that sets
`NEXTEST_PROFILE` or `NEXTEST_LOG` describes what its own test needs instead of reconfiguring the
runner.

## Ignored tests

An `#[ignore]`d test is listed rather than hidden, and reported as skipped when Buck2 asks for it.
A row Buck2 never shows would be indistinguishable from a test that does not exist.

## What it does and does not do

Per-test process isolation, [retries](../features/retries.md),
[slow-test handling](../features/slow-tests.md), and
[leak detection](../features/leaky-tests.md) all work as they do under Cargo, because they happen
inside the action Buck2 ran.

Some limits follow from Buck2 owning the run:

* **Rust targets only.** Nextest lists and runs tests over the libtest protocol.
* **Nothing that spans tests.** Test groups, global fail-fast, partitioning, and a run-level JUnit
  report have no meaning when each test is a separate action. Buck2's own scheduling replaces them.
* **A test binary is listed once per run, and again for each of its tests.** The pipeline enumerates
  before it runs, so each per-test action re-lists its binary. This is the cost of running the real
  pipeline per test.
* **No `buck2 test -- <nextest args>` passthrough.** Buck2 builds the command line; configure
  nextest through the toolchain and `.config/nextest.toml` instead.

## Buck2 support

`InternalRunnerTestInfo` landed in Buck2 in June 2026, but does not yet work in open-source Buck2.
Two bugs affect any target using it, neither of them nextest's:

* `buck2 test` fails while tearing the run down, after every result has already been reported
  correctly. The internal runner's orchestrator is dropped before the results channel is drained,
  and its `Drop` poisons the channel. Reported as
  [facebook/buck2#1479](https://github.com/facebook/buck2/issues/1479), fixed by
  [#1461](https://github.com/facebook/buck2/pull/1461).
* A failing test still exits zero. The exit code comes from the external test executor, which never
  saw these tests, so failures never reach it — a red run reports success.

Both reproduce with a rule that returns `InternalRunnerTestInfo` and runs `/bin/echo`, with nextest
nowhere in the picture. Against a Buck2 carrying both fixes, the example project in
`buck2-nextest/buck/` reports `Pass 6, Skip 1`, and a failing test exits 32.

## An example

The nextest repository contains a complete, runnable Buck2 project at `buck2-nextest/buck/`, with
the rule library it uses in `buck2-nextest/buck/nextest/`.

`buck2-nextest` is also the reference implementation of nextest's
[build system integration contract](../design/architecture/build-system-integration.md), for anyone
looking to drive nextest from another build system.
