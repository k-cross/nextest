// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! What a build system supplies to a session.

use camino::Utf8PathBuf;
use iddqd::IdOrdMap;
use nextest_metadata::BuildPlatform;
use nextest_runner::{
    cargo_config::EnvironmentMap,
    list::{BinaryList, ListProgressOptions, PackageInfo},
    partition::PartitionerBuilder,
    reuse_build::PathMapper,
    test_filter::FilterBound,
};
use std::sync::Arc;

/// What a build system supplies to nextest: the test binaries it built, and
/// the world they live in.
///
/// This is the integration contract's input. Everything downstream --
/// filtering, listing, running, and reporting -- is derived from these fields
/// plus nextest's own configuration.
#[derive(Debug)]
pub struct SessionInputs<'a> {
    /// The test binaries, with their build metadata.
    pub binary_list: Arc<BinaryList>,

    /// The package each binary belongs to.
    ///
    /// Every binary's `package_id` must name an entry here.
    pub packages: &'a IdOrdMap<PackageInfo>,

    /// The workspace or project root.
    ///
    /// Tests see this as an absolute `NEXTEST_WORKSPACE_ROOT`, and
    /// configuration is read from `.config/nextest.toml` under it.
    pub workspace_root: Utf8PathBuf,

    /// Extra environment variables for test processes.
    ///
    /// [`EnvironmentMap::empty`] if the build system has no such source.
    pub env: EnvironmentMap,

    /// A remapping for paths recorded at build time, for when the artifacts
    /// have moved since.
    ///
    /// [`PathMapper::noop`] if paths are already where the binaries are.
    pub path_mapper: PathMapper,
}

/// Options for building the test list.
#[derive(Debug)]
pub struct TestListOptions<'a> {
    /// Partitions the run across several invocations, if requested.
    pub partitioner_builder: Option<&'a PartitionerBuilder>,

    /// Restricts the list to one build platform.
    pub platform_filter: Option<BuildPlatform>,

    /// What filtersets are bounded by: the profile's default filter, or
    /// everything.
    pub filter_bound: FilterBound,

    /// The number of threads to list tests with.
    pub list_threads: usize,

    /// How to show progress while listing.
    pub progress: ListProgressOptions,
}
