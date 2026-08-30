// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The command line Buck2's rule builds.
//!
//! Both commands are assembled by the `nextest_test` rule and run by Buck2 as
//! ordinary actions, so this interface is between the rule and this binary
//! rather than something a person types. The one part that has to be exactly
//! right is the shape of the run command: Buck2 appends the test's `filter` as
//! the final argument, so the command the rule emits ends with a bare
//! `--test-name` and the appended value binds to it.

use crate::{
    convert::{TargetInput, to_binary_list},
    errors::Result,
    pipeline::Context,
    project_root,
};
use camino::Utf8PathBuf;
use clap::{Args, Parser, Subcommand};
use nextest_session::WriteStr;

/// A nextest client for Buck2's internal test runner.
#[derive(Debug, Parser)]
#[command(name = "buck2-nextest", version, about, long_about = None)]
pub struct App {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List the tests in a test binary, as JSON for Buck2's listing callback.
    List(ListArgs),

    /// Run one test from a test binary, as JSON for Buck2's result callback.
    Run(RunArgs),
}

/// The target both commands operate on, as the rule described it.
#[derive(Debug, Args)]
struct TargetArgs {
    /// The target's label, which becomes its nextest binary ID.
    #[arg(long, value_name = "LABEL")]
    label: String,

    /// The target's package path within the project, empty for the root
    /// package.
    #[arg(long, value_name = "PATH", default_value = "")]
    package_path: String,

    /// The test binary to list or run.
    #[arg(long, value_name = "PATH")]
    program: Utf8PathBuf,

    /// An argument to pass to the test binary before nextest's own.
    ///
    /// These come from the harness command the prelude rule built, so they are
    /// usually flags; `allow_hyphen_values` is what lets one be passed as a
    /// value rather than being mistaken for an argument of this binary.
    #[arg(long = "arg", value_name = "ARG", allow_hyphen_values = true)]
    args: Vec<String>,

    /// The Buck2 project root.
    ///
    /// Defaults to the directory the action runs in, which is the project root
    /// unless the rule turned `run_from_project_root` off.
    #[arg(long, value_name = "PATH")]
    project_root: Option<Utf8PathBuf>,

    /// The nextest profile to use.
    #[arg(long, short = 'P', env = "NEXTEST_PROFILE", value_name = "PROFILE")]
    profile: Option<String>,

    /// A nextest configuration file to read instead of the default.
    #[arg(long, value_name = "PATH")]
    config_file: Option<Utf8PathBuf>,
}

#[derive(Debug, Args)]
struct ListArgs {
    #[command(flatten)]
    target: TargetArgs,
}

#[derive(Debug, Args)]
struct RunArgs {
    #[command(flatten)]
    target: TargetArgs,

    /// The test to run, named exactly.
    ///
    /// Buck2 appends this as the final argument of the command, so the rule
    /// emits the flag with no value and Buck2 supplies it. The value is a test
    /// name this binary itself listed, and `allow_hyphen_values` keeps that
    /// round trip total rather than resting on test names never looking like
    /// flags.
    #[arg(long, value_name = "NAME", allow_hyphen_values = true)]
    test_name: String,
}

impl App {
    /// Runs the requested command, returning the process exit code.
    pub fn exec(self, writer: &mut dyn WriteStr, cli_args: Vec<String>) -> Result<i32> {
        match self.command {
            Command::List(args) => {
                let cx = args.target.into_context()?;
                crate::list::list(&cx, writer)?;
                Ok(0)
            }
            Command::Run(args) => {
                let test_name = args.test_name;
                let cx = args.target.into_context()?;
                crate::run_one::run_one(&cx, &test_name, cli_args, writer)
            }
        }
    }
}

impl TargetArgs {
    /// Resolves the project root and converts the target into pipeline inputs.
    fn into_context(self) -> Result<Context> {
        let cwd = project_root::current_dir()?;
        let project_root = project_root::resolve(self.project_root.as_deref(), &cwd)?;
        let binaries = to_binary_list(
            &TargetInput {
                label: &self.label,
                package_path: &self.package_path,
                program: &self.program,
                leading_args: &self.args,
                cwd: &cwd,
            },
            &project_root,
        )?;

        Ok(Context {
            label: self.label,
            binaries,
            project_root,
            profile_name: self.profile,
            config_file: self.config_file,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parse(args: &[&str]) -> App {
        App::try_parse_from(args).expect("arguments parse")
    }

    /// The shape the rule emits: `--test-name` last, with Buck2's appended
    /// filter binding to it.
    #[test]
    fn buck2_appends_the_filter_as_the_test_name() {
        let app = parse(&[
            "buck2-nextest",
            "run",
            "--label",
            "root//:demo",
            "--program",
            "out/demo",
            "--test-name",
            "tests::adds_two_numbers",
        ]);

        let Command::Run(args) = app.command else {
            panic!("expected the run command");
        };
        assert_eq!(args.test_name, "tests::adds_two_numbers");
        assert_eq!(args.target.label, "root//:demo");
        assert_eq!(args.target.program, "out/demo");
    }

    /// A test name that looks like a flag still binds as a value, since Buck2
    /// appends it positionally after `--test-name`.
    #[test]
    fn a_test_name_may_look_like_a_flag() {
        let app = parse(&[
            "buck2-nextest",
            "run",
            "--label",
            "root//:demo",
            "--program",
            "out/demo",
            "--test-name",
            "--not-a-flag",
        ]);

        let Command::Run(args) = app.command else {
            panic!("expected the run command");
        };
        assert_eq!(args.test_name, "--not-a-flag");
    }

    #[test]
    fn leading_args_accumulate_in_order() {
        let app = parse(&[
            "buck2-nextest",
            "list",
            "--label",
            "root//app:tests",
            "--package-path",
            "app",
            "--program",
            "out/tests",
            "--arg",
            "--first",
            "--arg",
            "--second",
        ]);

        let Command::List(args) = app.command else {
            panic!("expected the list command");
        };
        assert_eq!(args.target.args, vec!["--first", "--second"]);
        assert_eq!(args.target.package_path, "app");
    }

    /// The root package's path is empty, and leaving the flag off means the
    /// same thing as passing it empty.
    #[test]
    fn the_package_path_defaults_to_the_root_package() {
        let app = parse(&[
            "buck2-nextest",
            "list",
            "--label",
            "root//:demo",
            "--program",
            "out/demo",
        ]);

        let Command::List(args) = app.command else {
            panic!("expected the list command");
        };
        assert_eq!(args.target.package_path, "");
    }

    #[test]
    fn the_toolchain_may_pin_a_profile_and_config_file() {
        let app = parse(&[
            "buck2-nextest",
            "list",
            "--label",
            "root//:demo",
            "--program",
            "out/demo",
            "-P",
            "ci",
            "--config-file",
            "config/nextest.toml",
        ]);

        let Command::List(args) = app.command else {
            panic!("expected the list command");
        };
        assert_eq!(args.target.profile.as_deref(), Some("ci"));
        assert_eq!(
            args.target.config_file.as_ref().map(|path| path.as_str()),
            Some("config/nextest.toml")
        );
    }

    #[test]
    fn a_program_is_required() {
        App::try_parse_from(["buck2-nextest", "list", "--label", "root//:demo"])
            .expect_err("the program is required");
    }
}
