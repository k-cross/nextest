// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Turning Buck2's specs into targets nextest can run.
//!
//! A spec's command and environment are not directly runnable: Buck2 may send
//! opaque handles in place of values that are expensive or awkward to
//! serialise, and it does not say in the spec where the test should run. The
//! `PrepareForLocalExecution` RPC resolves all three -- it takes the spec's
//! values back, handles and all, and returns a full argument vector, a working
//! directory, and a verbatim environment.
//!
//! This is the reason the gRPC path supports handles where the file-based path
//! must reject them: resolving one means asking Buck2, and only this path has
//! anything to ask.

use crate::{
    errors::{ExpectedError, Result},
    proto::{
        ArgValue, ArgValueContent, ConfiguredTargetHandle, EnvironmentVariable, ExternalRunnerSpec,
        PrepareForLocalExecutionRequest, TestExecutable, TestStage, arg_value_content,
        test_orchestrator_client::TestOrchestratorClient, test_stage,
    },
    spec::{Buck2TestTarget, ConfiguredTarget, SUPPORTED_TEST_TYPE},
};
use camino::{Utf8Path, Utf8PathBuf};
use std::collections::BTreeMap;
use tonic::transport::Channel;

/// A target ready to run, plus what Buck2 said about it that nextest models
/// elsewhere.
///
/// Neither of the extra fields belongs on [`Buck2TestTarget`]: the handle means
/// nothing outside this protocol, and the project root is a property of the
/// session rather than of one target. Both are carried alongside instead.
#[derive(Clone, Debug)]
pub(super) struct PreparedTarget {
    /// The target, in the same shape the file-based path produces.
    pub(super) target: Buck2TestTarget,

    /// The handle to report results against.
    pub(super) handle: ConfiguredTargetHandle,

    /// The project root this target implies, if Buck2 said where it runs.
    pub(super) project_root: Option<Utf8PathBuf>,
}

/// Resolves every spec into a runnable target.
///
/// One round trip per target, in order. Buck2 has the answers in hand by this
/// point -- it built the targets before sending them -- so these are quick, and
/// issuing them in sequence keeps the failure that stops the run attributable
/// to the target that caused it.
pub(super) async fn prepare_all(
    client: &mut TestOrchestratorClient<Channel>,
    specs: Vec<ExternalRunnerSpec>,
) -> Result<Vec<PreparedTarget>> {
    let mut prepared = Vec::with_capacity(specs.len());
    for spec in specs {
        prepared.push(prepare_one(client, spec).await?);
    }
    Ok(prepared)
}

async fn prepare_one(
    client: &mut TestOrchestratorClient<Channel>,
    spec: ExternalRunnerSpec,
) -> Result<PreparedTarget> {
    let target = spec
        .target
        .clone()
        .ok_or_else(|| ExpectedError::SpecMissingField {
            field: "target",
            label: "<unknown>".to_owned(),
        })?;
    let configured = ConfiguredTarget {
        cell: target.cell.clone(),
        package: target.package.clone(),
        target: target.target.clone(),
        configuration: Some(target.configuration.clone()).filter(|c| !c.is_empty()),
    };
    let label = configured.label();

    if spec.test_type != SUPPORTED_TEST_TYPE {
        return Err(ExpectedError::UnsupportedTestType {
            label,
            test_type: spec.test_type,
        });
    }

    let handle = target
        .handle
        .ok_or_else(|| ExpectedError::SpecMissingField {
            field: "target.handle",
            label: label.clone(),
        })?;

    let request = PrepareForLocalExecutionRequest {
        test_executable: Some(TestExecutable {
            // Buck2 uses the stage for bookkeeping; nextest runs the same
            // binary to list and to execute, so it asks once, as a listing.
            stage: Some(TestStage {
                item: Some(test_stage::Item::Listing(test_stage::Listing {
                    suite: label.clone(),
                    cacheable: false,
                })),
            }),
            target: Some(handle),
            cmd: spec.command.into_iter().map(spec_arg).collect(),
            pre_create_dirs: Vec::new(),
            env: spec
                .env
                .into_iter()
                .map(|(key, value)| EnvironmentVariable {
                    key,
                    value: Some(spec_arg(value)),
                })
                .collect(),
        }),
        required_local_resources: Vec::new(),
    };

    let response = client
        .prepare_for_local_execution(request)
        .await
        .map_err(|status| ExpectedError::PrepareForLocalExecutionError {
            label: label.clone(),
            status: Box::new(status),
        })?
        .into_inner();

    let result = response
        .result
        .ok_or_else(|| ExpectedError::SpecMissingField {
            field: "PrepareForLocalExecutionResponse.result",
            label: label.clone(),
        })?;

    let mut cmd = result.cmd.into_iter();
    let program = cmd.next().ok_or_else(|| ExpectedError::EmptyCommand {
        label: label.clone(),
    })?;

    // Buck2 is authoritative about where a test runs, so an empty string means
    // "it did not say" rather than "the filesystem root".
    let cwd: Option<Utf8PathBuf> = Some(result.cwd)
        .filter(|cwd| !cwd.is_empty())
        .map(Into::into);
    let project_root = cwd
        .as_deref()
        .and_then(|cwd| project_root_from(cwd, &target.package_project_relative_path));

    Ok(PreparedTarget {
        target: Buck2TestTarget {
            label,
            target: configured,
            program,
            leading_args: cmd.collect(),
            env: result
                .env
                .into_iter()
                .map(|var| (var.key, var.value))
                .collect::<BTreeMap<_, _>>(),
            cwd,
        },
        handle,
        project_root,
    })
}

/// Works out the project root from where Buck2 says a target runs.
///
/// Buck2 does not name the project root on the executor's command line, and
/// nothing else in the protocol states it outright. What every target does carry
/// is a working directory -- which is either the project root or the target's own
/// package directory inside it, depending on the target's
/// `run_from_project_root` -- along with that package's path relative to the
/// root. Removing the second from the first covers both cases: the prelude's
/// `rust_test` runs from the project root, where the package path is not a
/// suffix and nothing is removed.
///
/// This is why the root is taken from Buck2 rather than from the process's own
/// working directory, which is Buck2's output directory rather than the project.
///
/// A project root that happens to end with the same names as one of its own
/// package paths has them removed when it should not be. Every target is asked
/// and a disagreement is reported rather than acted on, which catches that
/// whenever another target implies the true root.
///
/// Returns `None` for a working directory that is not absolute, since a root
/// derived from one would be no better than the relative path it came from.
fn project_root_from(cwd: &Utf8Path, package_path: &str) -> Option<Utf8PathBuf> {
    if !cwd.is_absolute() {
        return None;
    }

    // Compared component-wise: a textual suffix would match half a directory
    // name, and `package_path` is always `/`-separated while `cwd` may not be.
    let package_path = Utf8Path::new(package_path);
    let mut root = cwd.to_owned();
    for component in package_path.components().rev() {
        match root.file_name() {
            Some(name) if name == component.as_str() => {
                root.pop();
            }
            // The package path is not where this target runs, so the working
            // directory is the project root itself.
            _ => return Some(cwd.to_owned()),
        }
    }

    Some(root)
}

/// Wraps a spec value as an argument, leaving handles for Buck2 to resolve.
fn spec_arg(value: crate::proto::ExternalRunnerSpecValue) -> ArgValue {
    ArgValue {
        content: Some(ArgValueContent {
            value: Some(arg_value_content::Value::SpecValue(value)),
        }),
        format: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    /// A target that runs from the project root, which is what the prelude's
    /// `rust_test` does: the package path is not where it runs, so it is left on.
    #[test_case("/project", "" ; "root package")]
    #[test_case("/project", "sub" ; "one component")]
    #[test_case("/project", "app/tests" ; "several components")]
    fn a_root_working_directory_is_the_project_root(cwd: &str, package_path: &str) {
        assert_eq!(
            project_root_from(Utf8Path::new(cwd), package_path).as_deref(),
            Some(Utf8Path::new("/project"))
        );
    }

    /// A target that runs from its own package directory, which is what
    /// `run_from_project_root = False` gives.
    #[test_case("/project/sub", "sub" ; "one component")]
    #[test_case("/project/app/tests", "app/tests" ; "several components")]
    fn a_package_working_directory_has_its_package_removed(cwd: &str, package_path: &str) {
        assert_eq!(
            project_root_from(Utf8Path::new(cwd), package_path).as_deref(),
            Some(Utf8Path::new("/project"))
        );
    }

    /// Only whole directory names count, or a package named `test` would eat
    /// half of a directory named `tests`.
    #[test]
    fn only_whole_components_are_removed() {
        assert_eq!(
            project_root_from(Utf8Path::new("/project/tests"), "test").as_deref(),
            Some(Utf8Path::new("/project/tests"))
        );
    }

    /// Running short of a root means the package path was not a suffix after
    /// all, so the working directory stands.
    #[test]
    fn a_package_deeper_than_the_directory_is_not_removed() {
        assert_eq!(
            project_root_from(Utf8Path::new("/tests"), "app/tests").as_deref(),
            Some(Utf8Path::new("/tests"))
        );
    }

    #[test]
    fn a_relative_working_directory_implies_nothing() {
        assert_eq!(project_root_from(Utf8Path::new("project"), "sub"), None);
        assert_eq!(project_root_from(Utf8Path::new("."), ""), None);
    }
}
