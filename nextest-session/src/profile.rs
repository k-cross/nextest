// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Profile evaluation, and what it prepares on disk.

use crate::errors::StoreDirCreateError;
use nextest_runner::{
    config::core::{EarlyProfile, EvaluatableProfile},
    platform::BuildPlatforms,
};
use std::fs;

/// Creates the store directory if the profile writes a JUnit report.
///
/// Idempotent. [`evaluate_profile`] calls this itself; it is also exposed on
/// its own for a frontend that wants the failure surfaced before doing more
/// expensive work, such as a build.
pub fn create_junit_store_dir(profile: &EarlyProfile<'_>) -> Result<(), StoreDirCreateError> {
    if profile.has_junit() {
        let store_dir = profile.store_dir();
        fs::create_dir_all(store_dir).map_err(|error| StoreDirCreateError {
            store_dir: store_dir.to_owned(),
            error,
        })?;
    }
    Ok(())
}

/// Applies build platforms to a profile, making it evaluatable.
///
/// Also creates the profile's store directory if it writes a JUnit report, so
/// that the report does not fail at the end of the run for want of a
/// directory.
pub fn evaluate_profile<'cfg>(
    profile: EarlyProfile<'cfg>,
    build_platforms: &BuildPlatforms,
) -> Result<EvaluatableProfile<'cfg>, StoreDirCreateError> {
    create_junit_store_dir(&profile)?;
    Ok(profile.apply_build_platforms(build_platforms))
}
