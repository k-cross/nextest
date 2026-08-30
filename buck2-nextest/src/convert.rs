// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Converting a Buck2 test target into nextest's types.
//!
//! Nextest models a test suite as a binary belonging to a package, which is a
//! Cargo shape. Buck2's equivalent is a configured target label, so the target
//! becomes both a [`RustTestBinary`] and the [`PackageInfo`] it refers to.
//!
//! Buck2's internal runner works one target at a time -- each action it runs
//! covers a single test binary -- so unlike a whole-run integration, there is
//! exactly one of each to build here.

use crate::errors::{ExpectedError, Result};
use camino::{Utf8Path, Utf8PathBuf};
use iddqd::IdOrdMap;
use nextest_session::{
    BinaryList, BinaryListState, BuildPlatform, BuildPlatforms, PackageId, PackageInfo,
    RustBinaryId, RustBuildMeta, RustTestBinary, RustTestBinaryKind, TestBinaryInvocation,
};
use semver::Version;

/// The name Buck2 uses for its build files.
///
/// `PackageInfo::manifest_path` points at the file the target was declared in,
/// the way a Cargo package's manifest path points at its `Cargo.toml`.
const BUCK_FILE_NAME: &str = "BUCK";

/// The one test target this invocation is about, as the rule described it.
#[derive(Clone, Debug)]
pub(crate) struct TargetInput<'a> {
    /// The target's label, e.g. `root//app/tests:zebra`.
    pub(crate) label: &'a str,

    /// The target's package path within the project, e.g. `app/tests`.
    ///
    /// Empty for a target in the root package.
    pub(crate) package_path: &'a str,

    /// The test binary to run.
    pub(crate) program: &'a Utf8Path,

    /// Arguments that belong before nextest's own, from the harness command.
    pub(crate) leading_args: &'a [String],

    /// The directory Buck2 ran this action in, which is where the test runs.
    pub(crate) cwd: &'a Utf8Path,
}

/// A [`BinaryList`] plus the package its binary refers to.
///
/// The two travel together because `RustTestArtifact::from_binary_list` looks
/// packages up by ID, and the map must outlive the resulting `TestList`.
#[derive(Debug)]
pub(crate) struct Buck2BinaryList {
    /// The binary to list or run.
    pub(crate) binary_list: BinaryList,

    /// The package the binary belongs to, keyed by the target label.
    pub(crate) packages: IdOrdMap<PackageInfo>,
}

/// Converts a Buck2 target into a single-binary list.
///
/// `project_root` is the directory a relative program path resolves against.
pub(crate) fn to_binary_list(
    input: &TargetInput<'_>,
    project_root: &Utf8Path,
) -> Result<Buck2BinaryList> {
    // No Cargo configuration to consult, so the host is the only platform
    // there is anything to say about.
    let build_platforms = BuildPlatforms::new_with_no_target().map_err(|error| {
        ExpectedError::HostPlatformDetectError {
            error: Box::new(error),
        }
    })?;

    let package_dir = package_dir(project_root, input.package_path);
    let name = target_name(input.label).to_owned();
    let package_id = PackageId::new(input.label.to_owned());

    let binary = RustTestBinary {
        // Buck2 targets are identified by their label. Using it as the binary
        // ID keeps nextest's output and `binary_id()` filtersets speaking
        // Buck2's vocabulary rather than a synthesized Cargo-style name.
        id: RustBinaryId::new(input.label),
        path: resolve_path(project_root, input.program),
        package_id: input.label.to_owned(),
        // Buck2 has no lib/bin/test distinction of Cargo's sort. Reporting
        // these as `test` keeps `kind(test)` filtersets meaningful.
        kind: RustTestBinaryKind::TEST,
        name: name.clone(),
        build_platform: BuildPlatform::Target,
        invocation: TestBinaryInvocation {
            leading_args: input.leading_args.to_vec(),
            // The rule's `env` is the action's environment, which this process
            // already has and passes on to the test it spawns.
            env: Default::default(),
            // Buck2 runs each action from the project root (see
            // `run_from_project_root`), and the paths it hands a test through
            // the environment -- `$(location ...)` and friends -- are relative
            // to that root. So the test has to run where Buck2 put us, or none
            // of them resolve. Nextest reports this same directory as
            // `CARGO_MANIFEST_DIR`.
            cwd: Some(input.cwd.to_owned()),
        },
    };

    let mut packages = IdOrdMap::new();
    packages.insert_overwrite(PackageInfo {
        id: package_id,
        name,
        version: Version::new(0, 0, 0),
        authors: Vec::new(),
        description: None,
        homepage: None,
        license: None,
        license_file: None,
        repository: None,
        minimum_rust_version: None,
        manifest_path: package_dir.join(BUCK_FILE_NAME),
    });

    // Buck2 does not uplift artifacts or run Cargo build scripts, so the build
    // metadata is empty apart from the platforms. `dylib_paths()` then yields
    // just the rustc libdirs, which is what a Buck2-built test binary needs.
    let rust_build_meta: RustBuildMeta<BinaryListState> =
        RustBuildMeta::new(project_root, project_root, build_platforms);

    Ok(Buck2BinaryList {
        binary_list: BinaryList {
            rust_build_meta,
            rust_binaries: vec![binary],
        },
        packages,
    })
}

/// Returns the bare target name from a label.
///
/// A Buck2 label is `cell//package:name`, so the name is whatever follows the
/// last colon. A label without one is not something Buck2 produces; treating
/// the whole string as the name keeps this total rather than fallible, since
/// nothing downstream is harmed by a name that is merely unusual.
fn target_name(label: &str) -> &str {
    match label.rsplit_once(':') {
        Some((_, name)) => name,
        None => label,
    }
}

/// Returns the directory a Buck2 package lives in.
///
/// The root package's path is the empty string. Joining that on would leave a
/// trailing separator, which is harmless to run in but shows up verbatim in
/// nextest's output, so it is special-cased.
fn package_dir(project_root: &Utf8Path, package: &str) -> Utf8PathBuf {
    if package.is_empty() {
        project_root.to_owned()
    } else {
        project_root.join(package)
    }
}

/// Resolves a possibly-relative program path against the project root.
fn resolve_path(project_root: &Utf8Path, path: &Utf8Path) -> Utf8PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        project_root.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(label: &str, package_path: &str, program: &str) -> Buck2BinaryList {
        let args = vec!["--flag".to_owned()];
        to_binary_list(
            &TargetInput {
                label,
                package_path,
                program: Utf8Path::new(program),
                leading_args: &args,
                cwd: Utf8Path::new("/project"),
            },
            Utf8Path::new("/project"),
        )
        .expect("conversion succeeds")
    }

    #[test]
    fn the_label_becomes_the_binary_id_and_package_id() {
        let converted = convert("root//app/tests:zebra", "app/tests", "buck-out/gen/zebra");
        let binary = &converted.binary_list.rust_binaries[0];

        assert_eq!(binary.id.as_str(), "root//app/tests:zebra");
        assert_eq!(binary.package_id, "root//app/tests:zebra");
        assert_eq!(binary.name, "zebra");
        assert_eq!(binary.kind, RustTestBinaryKind::TEST);
    }

    #[test]
    fn relative_programs_resolve_against_the_project_root() {
        let converted = convert("root//app/tests:zebra", "app/tests", "buck-out/gen/zebra");
        assert_eq!(
            converted.binary_list.rust_binaries[0].path,
            "/project/buck-out/gen/zebra"
        );
    }

    #[test]
    fn absolute_programs_are_left_alone() {
        let converted = convert("root//app:aardvark", "app", "/abs/path/aardvark");
        assert_eq!(
            converted.binary_list.rust_binaries[0].path,
            "/abs/path/aardvark"
        );
    }

    /// A test runs where Buck2 ran the action, not in its package directory:
    /// the paths Buck2 hands it are relative to the project root.
    #[test]
    fn cwd_is_where_buck2_ran_the_action() {
        let converted = convert("root//app/tests:zebra", "app/tests", "buck-out/gen/zebra");
        let binary = &converted.binary_list.rust_binaries[0];
        assert_eq!(
            binary.invocation.cwd.as_deref(),
            Some(Utf8Path::new("/project"))
        );
        assert_eq!(binary.invocation.leading_args, vec!["--flag"]);

        // The manifest path still names the package, which is where the target
        // was declared.
        let package_id = PackageId::new("root//app/tests:zebra".to_owned());
        let package = converted.packages.get(&package_id).expect("present");
        assert_eq!(package.manifest_path, "/project/app/tests/BUCK");
    }

    /// The root package's manifest path has no trailing separator left behind
    /// by joining an empty package path onto the project root.
    #[test]
    fn the_root_package_manifest_is_at_the_project_root() {
        let converted = convert("root//:demo", "", "buck-out/demo");
        let binary = &converted.binary_list.rust_binaries[0];
        assert_eq!(binary.name, "demo");

        let package_id = PackageId::new("root//:demo".to_owned());
        let package = converted.packages.get(&package_id).expect("present");
        assert_eq!(package.manifest_path, "/project/BUCK");
    }

    /// Building the test list looks the package up by the binary's package ID,
    /// so a mismatch here fails the run rather than producing a bad listing.
    #[test]
    fn the_binary_has_a_matching_package() {
        let converted = convert("root//app:aardvark", "app", "out/aardvark");
        let binary = &converted.binary_list.rust_binaries[0];
        let package_id = PackageId::new(binary.package_id.clone());
        let package = converted.packages.get(&package_id).expect("present");
        assert_eq!(package.name, binary.name);
    }

    #[test]
    fn a_label_without_a_colon_is_its_own_name() {
        assert_eq!(target_name("root//app:aardvark"), "aardvark");
        assert_eq!(target_name("root//:demo"), "demo");
        assert_eq!(target_name("weird"), "weird");
    }
}
