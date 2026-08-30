# Copyright (c) The nextest Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""A `rust_test` whose tests Buck2 runs one at a time, through nextest.

The stock `rust_test` surfaces a whole libtest binary as a single unit of work:
ten `#[test]` functions are one row in `buck2 test`, and one cache entry. This
rule compiles the very same harness -- it reuses the prelude rule's attributes
and implementation -- but returns `InternalRunnerTestInfo` in place of
`ExternalRunnerTestInfo`, which asks Buck2 to

1. run `buck2-nextest list` to discover the binary's tests,
2. run `buck2-nextest run ... --test-name <test>` once per discovered test, and
3. read each command's JSON through the callbacks below.

Buck2 keeps everything that spans tests: scheduling, concurrency, caching, and
the results UI. Nextest keeps everything within a test, because `run` drives
the real nextest pipeline -- the profile's retries, slow-test handling, leak
detection, and the environment a test sees.

The callbacks are pure Starlark and cannot run anything, so all the judgement
lives in `buck2-nextest` and they are left as `json.decode`.

Swapping the provider is what makes this opt-in per target. Buck2 checks for
`InternalRunnerTestInfo` before `ExternalRunnerTestInfo`, so a target built by
this rule never reaches an external test runner, and every other target is
untouched.
"""

load("@prelude//decls:rust_rules.bzl", _prelude_rust_test = "rust_test")
load(":nextest_toolchain.bzl", "NextestToolchainInfo")

def _parse_test_listing(listing_content: str) -> list[dict[str, str]]:
    # `buck2-nextest list` writes a JSON array of {"name", "filter"}. Empty
    # output means the listing failed, and Buck2 reports that itself.
    if not listing_content.strip():
        return []
    return json.decode(listing_content)

def _parse_test_result(stdout: str, stderr: str, exit_code: int) -> list[dict]:
    _ = stderr  # @unused -- nextest's own reporter writes here, for the log.
    _ = exit_code  # @unused -- the status is in the JSON; see below.

    # Returning nothing asks Buck2 to synthesize a pass or failure from the exit
    # code, which is the right answer when the run did not get far enough to say
    # anything. `buck2-nextest` keeps its exit code consistent with this JSON so
    # that the two agree either way.
    if not stdout.strip():
        return []
    return json.decode(stdout)

def _nextest_test_impl(ctx: AnalysisContext) -> list[Provider]:
    # Everything the prelude rule produces is kept except the provider that
    # would route this to an external runner, so `buck2 run`, the default
    # outputs, and rust-analyzer discovery all behave as they would for a plain
    # `rust_test`.
    providers = []
    external = None
    for provider in _prelude_rust_test.impl(ctx):
        if isinstance(provider, ExternalRunnerTestInfo):
            external = provider
        else:
            providers.append(provider)

    if external == None:
        fail("the prelude rust_test rule returned no ExternalRunnerTestInfo")

    toolchain = ctx.attrs._nextest_toolchain[NextestToolchainInfo]

    # The harness command is the test binary followed by whatever arguments the
    # prelude decided it needs. The binary stays an argument rather than being
    # turned into a string, so Buck2 keeps it as an input and materializes it.
    harness = list(external.command)
    common = [
        "--label",
        str(ctx.label.raw_target()),
        "--package-path",
        ctx.label.package,
        "--program",
        harness[0],
    ]
    for arg in harness[1:]:
        common += ["--arg", arg]
    if toolchain.profile != None:
        common += ["--profile", toolchain.profile]
    if toolchain.config_file != None:
        common += ["--config-file", toolchain.config_file]

    providers.append(InternalRunnerTestInfo(
        # Carried through so that `[test] use_internal_runner = rust` still
        # selects on the framework this target actually is. The constructor
        # spells this `type`, while the field it sets reads back as `test_type`.
        type = external.test_type,
        listing_command = [toolchain.nextest, "list"] + common,
        # Buck2 appends the chosen test's `filter` as the final argument, so
        # this ends with a bare `--test-name` for that value to bind to.
        command = [toolchain.nextest, "run"] + common + ["--test-name"],
        parse_test_listing = _parse_test_listing,
        parse_test_result = _parse_test_result,
        env = external.env,
        # Passed through so `buck2 test --include` and `--exclude` keep working.
        labels = external.labels,
        contacts = external.contacts,
        run_from_project_root = external.run_from_project_root,
        use_project_relative_paths = external.use_project_relative_paths,
        default_executor = external.default_executor,
        executor_overrides = external.executor_overrides,
        local_resources = external.local_resources,
        required_local_resources = external.required_local_resources,
        worker = external.worker,
    ))

    return providers

nextest_test = rule(
    impl = _nextest_test_impl,
    attrs = _prelude_rust_test.attrs | {
        "_nextest_toolchain": attrs.toolchain_dep(
            default = "toolchains//:nextest",
            providers = [NextestToolchainInfo],
        ),
    },
    uses_plugins = _prelude_rust_test.uses_plugins,
    supports_incoming_transition = _prelude_rust_test.supports_incoming_transition,
    doc = "A rust_test whose tests Buck2 discovers and runs one at a time, through nextest.",
)
