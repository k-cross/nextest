// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Listing the tests in one Buck2 test target.
//!
//! This is the `listing_command` Buck2 runs once per test target. What it
//! prints becomes the set of tests Buck2 knows about, so anything left out here
//! is never run, and every entry must be one the run mode can select again by
//! its `filter`.

use crate::{
    errors::{ExpectedError, Result},
    output::{ListedTest, display_name},
    pipeline::Context,
};
use nextest_session::{
    FilterBound, NextestRunMode, RunIgnored, TestFilter, TestFilterPatterns, WriteStr,
};

/// Lists the target's tests as the JSON Buck2's `parse_test_listing` reads.
pub(crate) fn list(cx: &Context, writer: &mut dyn WriteStr) -> Result<()> {
    let config = cx.load_config()?;
    let early_profile = cx.load_profile(&config)?;
    let profile = cx.evaluate_profile(early_profile)?;
    let ctx = cx.session_context();

    let filter = TestFilter::new(
        NextestRunMode::Test,
        RunIgnored::All,
        TestFilterPatterns::new(Vec::new()),
        Vec::new(),
    )
    .map_err(|error| ExpectedError::TestFilterBuildError { error })?;

    let session = cx.build_session(&ctx, &profile, &filter, FilterBound::DefaultSet)?;

    let listed: Vec<ListedTest> = session
        .test_list()
        .iter_tests()
        .filter(|test| test.test_info.filter_match.is_match())
        .map(|test| ListedTest {
            name: display_name(&cx.label, test.name),
            filter: test.name.to_string(),
        })
        .collect();

    let json = serde_json::to_string(&listed)
        .map_err(|error| ExpectedError::ResultSerializeError { error })?;

    writer
        .write_str(&json)
        .and_then(|()| writer.write_str("\n"))
        .and_then(|()| writer.write_str_flush())
        .map_err(|error| ExpectedError::WriteEventError { error })
}
