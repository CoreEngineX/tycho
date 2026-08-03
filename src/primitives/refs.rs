//! Detecting refnames that two different fetches would map onto one file.
//!
//! Loose refs are files, so on a case-insensitive volume a repository carrying both
//! `Feature` and `feature` maps two branches onto one path. Git errors on first
//! exposure and the *next* fetch is silent, clobbering the captured tip.

use crate::primitives::names::RefName;
use std::collections::BTreeMap;
use unicode_normalization::UnicodeNormalization;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collision {
    pub folded: String,
    pub names: Vec<RefName>,
}

/// Groups refnames that fold together, reporting only the groups larger than one.
///
/// Percent-encoding leaves the `<key>` portion of a destination refname pure ASCII,
/// so normalisation can only ever matter for the branch and tag tail, which the
/// refspec copies from the source unencoded.
#[must_use]
pub fn find_collisions(names: &[RefName]) -> Vec<Collision> {
    let mut groups: BTreeMap<String, Vec<RefName>> = BTreeMap::new();
    for name in names {
        groups
            .entry(fold(name.as_str()))
            .or_default()
            .push(name.clone());
    }
    groups
        .into_iter()
        .filter(|(_, group)| group.len() > 1)
        .map(|(folded, names)| Collision { folded, names })
        .collect()
}

fn fold(name: &str) -> String {
    name.nfc().collect::<String>().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::find_collisions;
    use crate::primitives::names::RefName;

    fn refs(names: &[&str]) -> Vec<RefName> {
        names
            .iter()
            .map(|name| RefName::parse(name).expect("valid refname"))
            .collect()
    }

    #[test]
    fn distinct_names_do_not_collide() {
        let names = refs(&["refs/heads/main", "refs/heads/dev", "refs/tags/v1.0"]);
        assert!(find_collisions(&names).is_empty());
    }

    #[test]
    fn case_only_differences_collide() {
        let names = refs(&["refs/heads/Feature", "refs/heads/feature"]);
        let found = find_collisions(&names);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].folded, "refs/heads/feature");
        assert_eq!(found[0].names, names);
    }

    #[test]
    fn composed_and_decomposed_forms_collide() {
        let composed = "refs/heads/caf\u{e9}";
        let decomposed = "refs/heads/cafe\u{301}";
        assert_ne!(composed, decomposed);
        let found = find_collisions(&refs(&[composed, decomposed]));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].names.len(), 2);
    }

    #[test]
    fn a_collision_reports_every_member() {
        let names = refs(&[
            "refs/heads/A",
            "refs/heads/a",
            "refs/heads/b",
            "refs/heads/B",
            "refs/heads/c",
        ]);
        let found = find_collisions(&names);
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|group| group.names.len() == 2));
    }
}
