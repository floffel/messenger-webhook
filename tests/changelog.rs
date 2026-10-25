//! The changelog is part of the release gate, not a file someone remembers to
//! edit: its newest heading must equal the version `Cargo.toml` publishes.
//!
//! `cargo test` therefore fails on the two ways a release goes wrong — a version
//! bump without an entry, and an entry that names a version the registry never
//! sees. The parser is deliberately strict about shape (`## [x.y.z] - YYYY-MM-DD`,
//! newest first, no duplicates) and deliberately tolerant about prose, because
//! the format is the contract and the words are not.

use std::collections::BTreeSet;
use std::fs;

/// A `## [x.y.z] - YYYY-MM-DD` section of the changelog.
struct Section {
    version: (u64, u64, u64),
    heading: String,
    body: String,
}

fn changelog() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/CHANGELOG.md");
    fs::read_to_string(path).expect("CHANGELOG.md is part of the repository")
}

/// `(major, minor, patch)` for a plain `x.y.z`, `None` for anything else.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Split the file into versioned sections, ignoring `## [Unreleased]`.
fn sections(text: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("## [") {
            let (label, _) = rest.split_once(']').expect("heading closes its bracket");
            if let Some(version) = parse_version(label) {
                out.push(Section {
                    version,
                    heading: line.to_string(),
                    body: String::new(),
                });
            } else {
                assert_eq!(label, "Unreleased", "unparsable changelog heading: {line}");
            }
            continue;
        }
        assert!(
            !line.starts_with("## "),
            "changelog heading must use the `## [version]` form: {line}"
        );
        if let Some(last) = out.last_mut() {
            last.body.push_str(line);
            last.body.push('\n');
        }
    }
    out
}

#[test]
fn newest_section_is_the_version_being_published() {
    let text = changelog();
    let sections = sections(&text);
    let newest = sections
        .first()
        .expect("the changelog has a released section");
    assert_eq!(
        newest.version,
        parse_version(env!("CARGO_PKG_VERSION")).expect("Cargo.toml version is x.y.z"),
        "newest changelog heading is `{}`, published version is {} — bump the changelog with the version",
        newest.heading,
        env!("CARGO_PKG_VERSION")
    );
}

#[test]
fn sections_are_newest_first_without_duplicates() {
    let text = changelog();
    let sections = sections(&text);
    let mut seen = BTreeSet::new();
    for section in &sections {
        assert!(
            seen.insert(section.version),
            "duplicate changelog section for {:?}",
            section.version
        );
    }
    for pair in sections.windows(2) {
        assert!(
            pair[0].version > pair[1].version,
            "sections out of order: `{}` comes before `{}`",
            pair[0].heading,
            pair[1].heading
        );
    }
}

#[test]
fn every_section_says_something() {
    let text = changelog();
    for section in sections(&text) {
        let bullets = section
            .body
            .lines()
            .filter(|line| line.starts_with("- "))
            .count();
        assert!(
            bullets > 0,
            "changelog section `{}` has no `- ` entry",
            section.heading
        );
        assert!(
            section.body.contains("### "),
            "changelog section `{}` has no change category",
            section.heading
        );
    }
}

#[test]
fn the_file_names_itself_a_changelog() {
    let text = changelog();
    let first = text.lines().next().expect("the file is not empty");
    assert_eq!(first, "# Changelog");
    assert!(
        text.contains("Keep a Changelog"),
        "the format is stated, so readers know which conventions apply"
    );
}

#[test]
fn breaking_entries_exist_where_the_commit_history_has_them() {
    // 0.2.0 moved verification into the adapter contract (`feat!`); the policy in
    // the README promises such a change is called out, so the entry must exist.
    let text = changelog();
    let section = sections(&text)
        .into_iter()
        .find(|section| section.version == (0, 2, 0))
        .expect("0.2.0 is documented");
    assert!(
        section.body.contains("### Breaking"),
        "0.2.0 carries breaking changes and must say so"
    );
}

#[test]
fn a_malformed_version_is_rejected() {
    assert_eq!(parse_version("1.2"), None);
    assert_eq!(parse_version("1.2.3.4"), None);
    assert_eq!(parse_version("1.2.3-rc.1"), None);
    assert_eq!(parse_version(""), None);
    assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
}
