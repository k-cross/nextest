// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Working out where the Buck2 project root is.
//!
//! Buck2 does not name the project root on the executor's command line, and
//! nothing else in the protocol states it outright, so it has to be derived.
//! Everything that reads configuration or reports paths depends on getting it
//! right: it is the directory `.config/nextest.toml` is looked up under, the
//! `NEXTEST_WORKSPACE_ROOT` tests see, and what the profile's store directory
//! hangs off.

use crate::errors::{ExpectedError, Result};
use camino::{Utf8Path, Utf8PathBuf};

/// Marks the root of a Buck2 project.
const BUCKROOT_FILE: &str = ".buckroot";

/// Marks the root of a Buck2 cell. The project root is a cell root too, but
/// cells nest, so this on its own does not identify the project.
const BUCKCONFIG_FILE: &str = ".buckconfig";

/// How strongly a directory claims to be the Buck2 project root.
///
/// Ordered weakest to strongest, so that two candidates can be compared.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum RootMarker {
    /// Nothing marks this directory.
    None,

    /// A `.buckconfig`: the root of some cell, which may or may not be the
    /// project.
    Cell,

    /// A `.buckroot`: the project root itself.
    Project,
}

/// Returns what, if anything, marks `dir` as a Buck2 root.
///
/// A directory that cannot be read is reported as unmarked. This is only ever
/// used to choose between candidates that were derived some other way, so a
/// wrong answer here costs the disambiguation rather than the root itself.
pub(crate) fn root_marker(dir: &Utf8Path) -> RootMarker {
    if dir.join(BUCKROOT_FILE).exists() {
        RootMarker::Project
    } else if dir.join(BUCKCONFIG_FILE).exists() {
        RootMarker::Cell
    } else {
        RootMarker::None
    }
}

/// Finds the project root by walking up from `start`.
///
/// The nearest `.buckroot` wins outright. Failing that, the outermost
/// `.buckconfig` does: cells nest, and the project encloses all of them.
///
/// Used when nothing Buck2 sent implies a root -- a run that selects no test
/// targets at all -- where the alternative is the executor's own working
/// directory, which is Buck2's output directory rather than the project.
pub(crate) fn find_project_root(start: &Utf8Path) -> Option<Utf8PathBuf> {
    let mut outermost_cell = None;
    for dir in start.ancestors() {
        match root_marker(dir) {
            RootMarker::Project => return Some(dir.to_owned()),
            RootMarker::Cell => outermost_cell = Some(dir.to_owned()),
            RootMarker::None => {}
        }
    }
    outermost_cell
}

/// Makes a project root absolute, so that everything derived from it is too.
///
/// Nextest passes the project root to each test process as
/// `NEXTEST_WORKSPACE_ROOT`, and the directory the test runs in as
/// `CARGO_MANIFEST_DIR`. A test resolves neither against the project: it runs
/// somewhere within it, so a relative root -- such as a bare `.` -- would name
/// wherever the test happens to be instead. `cargo-nextest` guarantees both are
/// absolute, and so does this.
///
/// Symlinks are deliberately left alone: a project root is stated rather than
/// discovered, and resolving one would leave these paths naming a place that
/// whoever stated it did not.
pub(crate) fn absolute_project_root(path: &Utf8Path) -> Result<Utf8PathBuf> {
    let absolute =
        std::path::absolute(path).map_err(|error| ExpectedError::ProjectRootAbsoluteError {
            path: path.to_owned(),
            error,
        })?;
    Utf8PathBuf::try_from(absolute).map_err(|error| ExpectedError::ProjectRootNonUtf8 {
        path: path.to_owned(),
        error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino_tempfile::Utf8TempDir;

    fn touch(dir: &Utf8Path, name: &str) {
        std::fs::create_dir_all(dir).expect("the directory is created");
        std::fs::write(dir.join(name), "").expect("the marker file is written");
    }

    #[test]
    fn markers_are_ranked_by_how_much_they_say() {
        let temp = Utf8TempDir::new().expect("a temporary directory");
        let unmarked = temp.path().join("unmarked");
        let cell = temp.path().join("cell");
        let project = temp.path().join("project");

        std::fs::create_dir_all(&unmarked).expect("the directory is created");
        touch(&cell, BUCKCONFIG_FILE);
        touch(&project, BUCKROOT_FILE);

        assert_eq!(root_marker(&unmarked), RootMarker::None);
        assert_eq!(root_marker(&cell), RootMarker::Cell);
        assert_eq!(root_marker(&project), RootMarker::Project);
        assert!(RootMarker::Project > RootMarker::Cell);
        assert!(RootMarker::Cell > RootMarker::None);
    }

    /// A `.buckroot` names the project outright, even with a cell between it
    /// and where the walk started.
    #[test]
    fn the_nearest_buckroot_wins() {
        let temp = Utf8TempDir::new().expect("a temporary directory");
        let root = temp.path().join("project");
        let cell = root.join("cell");
        let output = cell.join("buck-out/isolation");

        touch(&root, BUCKROOT_FILE);
        touch(&cell, BUCKCONFIG_FILE);
        std::fs::create_dir_all(&output).expect("the directory is created");

        assert_eq!(find_project_root(&output).as_deref(), Some(root.as_path()));
    }

    /// Without a `.buckroot`, the outermost cell is the best guess at the
    /// project, since cells nest inside it.
    #[test]
    fn the_outermost_buckconfig_wins_without_a_buckroot() {
        let temp = Utf8TempDir::new().expect("a temporary directory");
        let root = temp.path().join("project");
        let cell = root.join("cell");
        let output = cell.join("buck-out/isolation");

        touch(&root, BUCKCONFIG_FILE);
        touch(&cell, BUCKCONFIG_FILE);
        std::fs::create_dir_all(&output).expect("the directory is created");

        assert_eq!(find_project_root(&output).as_deref(), Some(root.as_path()));
    }

    #[test]
    fn an_unmarked_tree_implies_nothing() {
        let temp = Utf8TempDir::new().expect("a temporary directory");
        let output = temp.path().join("buck-out/isolation");
        std::fs::create_dir_all(&output).expect("the directory is created");

        assert_eq!(find_project_root(&output), None);
    }

    #[test]
    fn a_relative_root_is_made_absolute() {
        let root = absolute_project_root(Utf8Path::new(".")).expect("`.` is made absolute");
        assert!(root.is_absolute(), "got {root}");
    }
}
