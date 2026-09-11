// Copyright (c) The nextest Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Parsing filtersets against a profile's known groups.

use nextest_filtering::{
    Filterset, FiltersetKind, KnownGroups, ParseContext, errors::FiltersetParseErrors,
};

/// Parses a set of filterset inputs, reporting every bad one at once rather
/// than stopping at the first.
///
/// `known_groups` comes from the profile: `group()` is legal in a filterset,
/// so the set of valid group names must be known before one is compiled.
pub fn parse_filtersets(
    pcx: &ParseContext<'_>,
    inputs: &[String],
    kind: FiltersetKind,
    known_groups: &KnownGroups,
) -> Result<Vec<Filterset>, Vec<FiltersetParseErrors>> {
    let mut filtersets = Vec::with_capacity(inputs.len());
    let mut all_errors = Vec::new();
    for input in inputs {
        match Filterset::parse(input.clone(), pcx, kind, known_groups) {
            Ok(filterset) => filtersets.push(filterset),
            Err(errors) => all_errors.push(errors),
        }
    }

    if all_errors.is_empty() {
        Ok(filtersets)
    } else {
        Err(all_errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn no_groups() -> KnownGroups {
        KnownGroups::Known {
            custom_groups: HashSet::new(),
        }
    }

    #[test]
    fn valid_filtersets_parse_in_order() {
        let pcx = ParseContext::without_graph();
        let inputs = vec!["all()".to_owned(), "test(foo)".to_owned()];
        let filtersets = parse_filtersets(&pcx, &inputs, FiltersetKind::Test, &no_groups())
            .expect("both filtersets are valid");
        assert_eq!(filtersets.len(), 2);
    }

    #[test]
    fn every_invalid_filterset_is_reported() {
        let pcx = ParseContext::without_graph();
        let inputs = vec![
            "test(".to_owned(),
            "all()".to_owned(),
            "nonsense_predicate(foo)".to_owned(),
        ];
        let all_errors = parse_filtersets(&pcx, &inputs, FiltersetKind::Test, &no_groups())
            .expect_err("two of the filtersets are invalid");
        assert_eq!(
            all_errors.len(),
            2,
            "both bad filtersets are reported, not just the first"
        );
        assert_eq!(all_errors[0].input, "test(");
        assert_eq!(all_errors[1].input, "nonsense_predicate(foo)");
    }
}
