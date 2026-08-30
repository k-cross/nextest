# Copyright (c) The nextest Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""The toolchain that says where `buck2-nextest` is, and how to configure it.

Two rules, differing only in where the binary comes from:

* `system_nextest_toolchain` finds it on `PATH`, the way the prelude's demo
  toolchains find `rustc`. Nothing to build, but the binary is not an input to
  the actions that use it, so remote execution cannot see it.
* `nextest_toolchain` takes a target that produces the binary, which makes it a
  real input Buck2 materializes wherever the action runs.

`config_file` is a source rather than a path for the same reason: naming the
nextest configuration as an input is what lets it be found on a machine that
only has what the action declared.
"""

NextestToolchainInfo = provider(
    doc = "How to invoke `buck2-nextest`, and what to configure it with.",
    fields = {
        "config_file": provider_field(typing.Any, default = None),
        "nextest": provider_field(typing.Any, default = None),
        "profile": provider_field(typing.Any, default = None),
    },
)

def _system_nextest_toolchain_impl(ctx: AnalysisContext) -> list[Provider]:
    return [
        DefaultInfo(),
        NextestToolchainInfo(
            nextest = RunInfo(args = [ctx.attrs.nextest]),
            profile = ctx.attrs.profile,
            config_file = ctx.attrs.config_file,
        ),
    ]

system_nextest_toolchain = rule(
    impl = _system_nextest_toolchain_impl,
    attrs = {
        "config_file": attrs.option(attrs.source(), default = None),
        "nextest": attrs.string(default = "buck2-nextest"),
        "profile": attrs.option(attrs.string(), default = None),
    },
    is_toolchain_rule = True,
    doc = "A `buck2-nextest` found on PATH.",
)

def _nextest_toolchain_impl(ctx: AnalysisContext) -> list[Provider]:
    return [
        DefaultInfo(),
        NextestToolchainInfo(
            nextest = ctx.attrs.nextest[RunInfo],
            profile = ctx.attrs.profile,
            config_file = ctx.attrs.config_file,
        ),
    ]

nextest_toolchain = rule(
    impl = _nextest_toolchain_impl,
    attrs = {
        "config_file": attrs.option(attrs.source(), default = None),
        "nextest": attrs.exec_dep(providers = [RunInfo]),
        "profile": attrs.option(attrs.string(), default = None),
    },
    is_toolchain_rule = True,
    doc = "A `buck2-nextest` built or vendored as a target, materialized per action.",
)
