// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Package metadata required to list and run a test binary.
//!
//! Nextest's list and run phases need a small amount of information about the
//! package a test binary belongs to: an identity to key overrides and build
//! script outputs off, and the values behind the `CARGO_PKG_*` environment
//! variables that `cargo test` sets for every test process.
//!
//! Cargo-based callers derive this from a [`PackageMetadata`] obtained from a
//! `guppy::graph::PackageGraph`. Orchestrators for other build systems have no
//! package graph, so this type is the seam: it carries exactly the fields the
//! list and run phases consume, and nothing more.

use crate::list::BinaryList;
use camino::{Utf8Path, Utf8PathBuf};
use guppy::{
    PackageId,
    graph::{PackageGraph, PackageMetadata},
};
use iddqd::{IdOrdItem, IdOrdMap, id_upcast};
use semver::Version;
use std::collections::BTreeSet;

/// Information about the package a test binary belongs to.
///
/// See the [module-level documentation](self) for why this exists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageInfo {
    /// This is the package ID from `cargo metadata`. It is used to
    /// key build script output directories and to evaluate `package()`
    /// filtersets, and is otherwise opaque to nextest.
    pub id: PackageId,
    /// The name of the package (`CARGO_PKG_NAME`).
    pub name: String,
    /// The version of the package (`CARGO_PKG_VERSION` and friends).
    pub version: Version,
    /// The authors of the package (`CARGO_PKG_AUTHORS`).
    pub authors: Vec<String>,
    /// The description of the package (`CARGO_PKG_DESCRIPTION`).
    pub description: Option<String>,
    /// The homepage of the package (`CARGO_PKG_HOMEPAGE`).
    pub homepage: Option<String>,
    /// The license of the package (`CARGO_PKG_LICENSE`).
    pub license: Option<String>,
    /// The path to the package's license file (`CARGO_PKG_LICENSE_FILE`).
    pub license_file: Option<Utf8PathBuf>,
    /// The repository of the package (`CARGO_PKG_REPOSITORY`).
    pub repository: Option<String>,
    /// The minimum supported Rust version (`CARGO_PKG_RUST_VERSION`).
    pub minimum_rust_version: Option<Version>,
    /// The path to the package's manifest.
    pub manifest_path: Utf8PathBuf,
}

impl PackageInfo {
    /// Creates a `PackageInfo` from Cargo package metadata.
    pub fn from_package_metadata(package: &PackageMetadata<'_>) -> Self {
        Self {
            id: package.id().clone(),
            name: package.name().to_owned(),
            version: package.version().clone(),
            authors: package.authors().to_vec(),
            description: package.description().map(ToOwned::to_owned),
            homepage: package.homepage().map(ToOwned::to_owned),
            license: package.license().map(ToOwned::to_owned),
            license_file: package.license_file().map(ToOwned::to_owned),
            repository: package.repository().map(ToOwned::to_owned),
            minimum_rust_version: package.minimum_rust_version().cloned(),
            manifest_path: package.manifest_path().to_owned(),
        }
    }

    /// Builds a map of `PackageInfo` for every package in a Cargo package graph.
    ///
    /// This is the bridge Cargo-based callers use to satisfy
    /// [`RustTestArtifact::from_binary_list`](crate::list::RustTestArtifact::from_binary_list).
    pub fn map_from_graph(graph: &PackageGraph) -> IdOrdMap<Self> {
        graph
            .packages()
            .map(|package| Self::from_package_metadata(&package))
            .collect()
    }

    /// Builds a map of `PackageInfo` for the packages a binary list refers to.
    ///
    /// [`RustTestArtifact::from_binary_list`](crate::list::RustTestArtifact::from_binary_list)
    /// only looks up the packages its binaries belong to, so this is what
    /// Cargo-based callers want in preference to [`Self::map_from_graph`]:
    /// a workspace's test binaries typically name a small fraction of the
    /// packages in its graph.
    ///
    /// A package ID that is not in the graph is left out, and reported by
    /// `from_binary_list` against the binary that named it.
    pub fn map_from_binary_list(graph: &PackageGraph, binary_list: &BinaryList) -> IdOrdMap<Self> {
        binary_list
            .rust_binaries
            .iter()
            .map(|binary| binary.package_id.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter_map(|id| graph.metadata(&PackageId::new(id)).ok())
            .map(|package| Self::from_package_metadata(&package))
            .collect()
    }

    /// This is the directory containing the manifest.
    pub fn cwd(&self) -> &Utf8Path {
        self.manifest_path
            .parent()
            .unwrap_or_else(|| panic!("manifest path {} doesn't have a parent", self.manifest_path))
    }
}

impl IdOrdItem for PackageInfo {
    type Key<'a> = &'a PackageId;

    fn key(&self) -> Self::Key<'_> {
        &self.id
    }

    id_upcast!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cargo_config::TargetTriple,
        list::{
            BinaryListState, RustBuildMeta, RustTestBinary, TestBinaryInvocation,
            test_helpers::{PACKAGE_GRAPH_FIXTURE, PACKAGE_METADATA_ID},
        },
        platform::{BuildPlatforms, HostPlatform, PlatformLibdir},
    };
    use nextest_metadata::{BuildPlatform, RustBinaryId, RustTestBinaryKind};

    /// Every field must be carried over from the graph. The `CARGO_PKG_*`
    /// environment variables tests observe are derived from these, so a
    /// dropped field is a silent behavior change.
    #[test]
    fn from_package_metadata_preserves_fields() {
        for package in PACKAGE_GRAPH_FIXTURE.packages() {
            let info = PackageInfo::from_package_metadata(&package);

            assert_eq!(&info.id, package.id(), "id matches");
            assert_eq!(info.name, package.name(), "name matches");
            assert_eq!(&info.version, package.version(), "version matches");
            assert_eq!(info.authors, package.authors(), "authors match");
            assert_eq!(
                info.description.as_deref(),
                package.description(),
                "description matches"
            );
            assert_eq!(
                info.homepage.as_deref(),
                package.homepage(),
                "homepage matches"
            );
            assert_eq!(
                info.license.as_deref(),
                package.license(),
                "license matches"
            );
            assert_eq!(
                info.license_file.as_deref(),
                package.license_file(),
                "license file matches"
            );
            assert_eq!(
                info.repository.as_deref(),
                package.repository(),
                "repository matches"
            );
            assert_eq!(
                info.minimum_rust_version.as_ref(),
                package.minimum_rust_version(),
                "minimum rust version matches"
            );
            assert_eq!(
                info.manifest_path,
                package.manifest_path(),
                "manifest path matches"
            );
        }
    }

    /// `from_binary_list` looks packages up by ID, so the map must cover every
    /// package a test binary could belong to, not just workspace members.
    #[test]
    fn map_from_graph_covers_every_package() {
        let map = PackageInfo::map_from_graph(&PACKAGE_GRAPH_FIXTURE);

        assert_eq!(
            map.len(),
            PACKAGE_GRAPH_FIXTURE.package_count(),
            "map has an entry per package"
        );
        for package in PACKAGE_GRAPH_FIXTURE.packages() {
            assert!(
                map.get(package.id()).is_some(),
                "package {} is present in the map",
                package.id()
            );
        }
    }

    fn binary_list_naming(package_ids: &[&str]) -> BinaryList {
        let build_platforms = BuildPlatforms {
            host: HostPlatform {
                platform: TargetTriple::x86_64_unknown_linux_gnu().platform,
                libdir: PlatformLibdir::Available("/fake/libdir".into()),
            },
            target: None,
        };
        BinaryList {
            rust_build_meta: RustBuildMeta::<BinaryListState>::new(
                "/fake",
                "/fake",
                build_platforms,
            ),
            rust_binaries: package_ids
                .iter()
                .enumerate()
                .map(|(index, package_id)| RustTestBinary {
                    id: RustBinaryId::new(&format!("binary-{index}")),
                    path: format!("/fake/binary-{index}").into(),
                    package_id: (*package_id).to_owned(),
                    kind: RustTestBinaryKind::LIB,
                    name: format!("binary-{index}"),
                    build_platform: BuildPlatform::Target,
                    invocation: TestBinaryInvocation::empty(),
                })
                .collect(),
        }
    }

    /// The map must cover exactly the packages the binaries name, however many
    /// binaries share one, and must not carry the rest of the graph along.
    #[test]
    fn map_from_binary_list_covers_named_packages_only() {
        let map = PackageInfo::map_from_binary_list(
            &PACKAGE_GRAPH_FIXTURE,
            &binary_list_naming(&[PACKAGE_METADATA_ID, PACKAGE_METADATA_ID]),
        );

        assert_eq!(map.len(), 1, "the shared package is present once");
        assert!(
            map.get(&PackageId::new(PACKAGE_METADATA_ID)).is_some(),
            "the named package is present"
        );
        assert!(
            PACKAGE_GRAPH_FIXTURE.package_count() > 1,
            "the fixture has packages the binary does not name"
        );
    }

    /// A package ID the graph does not know is reported by `from_binary_list`
    /// against the binary that named it, so building the map must not fail.
    #[test]
    fn map_from_binary_list_skips_unknown_packages() {
        let map = PackageInfo::map_from_binary_list(
            &PACKAGE_GRAPH_FIXTURE,
            &binary_list_naming(&["not-a-real-package 1.0.0 (path+file:///nowhere)"]),
        );

        assert!(map.is_empty(), "the unknown package is left out");
    }

    /// Tests run in the directory containing the manifest.
    #[test]
    fn cwd_is_the_manifest_directory() {
        for package in PACKAGE_GRAPH_FIXTURE.packages() {
            let info = PackageInfo::from_package_metadata(&package);
            assert_eq!(
                info.cwd(),
                package
                    .manifest_path()
                    .parent()
                    .expect("manifest path has a parent"),
                "cwd is the manifest directory"
            );
        }
    }
}
