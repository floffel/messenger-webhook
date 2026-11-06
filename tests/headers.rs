//! Boundary and property tests for the pure `Headers` helpers.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use messenger_webhook::Headers;
use proptest::prelude::*;

fn header_name() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-zA-Z0-9!#$%&'*+.^_`|~-]{0,64}".prop_map(String::from),
        "\\PC{0,24}".prop_map(String::from),
    ]
}

fn insert_model(entries: &mut Vec<(String, String)>, name: &str, value: &str) {
    let name = name.to_ascii_lowercase();
    if let Some((_, existing)) = entries.iter_mut().find(|(key, _)| *key == name) {
        value.clone_into(existing);
    } else {
        entries.push((name, value.to_owned()));
    }
}

fn model_value<'a>(entries: &'a [(String, String)], name: &str) -> Option<&'a str> {
    entries
        .iter()
        .find(|(key, _)| key == &name.to_ascii_lowercase())
        .map(|(_, value)| value.as_str())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// `insert` lowercases names with ASCII rules, replaces a same-key entry,
    /// and leaves all other entries in place. `get` follows the same identity.
    #[test]
    fn insert_and_get_match_an_ascii_lowercase_last_wins_model(
        pairs in prop::collection::vec((header_name(), ".*"), 0..24),
    ) {
        let mut headers = Headers::new();
        let mut expected = Vec::new();

        for (name, value) in &pairs {
            headers.insert(name, value.clone());
            insert_model(&mut expected, name, value);
        }

        prop_assert_eq!(headers.len(), expected.len());
        prop_assert_eq!(headers.is_empty(), expected.is_empty());

        for (name, value) in &expected {
            prop_assert_eq!(headers.get(name), Some(value.as_str()));
            prop_assert_eq!(headers.get(&name.to_ascii_uppercase()), Some(value.as_str()));
        }

        for (name, _) in &pairs {
            let expected_value = model_value(&expected, name);
            prop_assert_eq!(headers.get(name), expected_value);
            prop_assert_eq!(headers.get(&name.to_ascii_lowercase()), expected_value);
        }
    }

    /// The constructor forms are exactly repeated `insert` calls, including
    /// duplicate keys and names that only differ by ASCII case.
    #[test]
    fn constructors_match_repeated_insert(
        pairs in prop::collection::vec((header_name(), ".*"), 0..24),
    ) {
        let mut expected = Headers::new();
        for (name, value) in &pairs {
            expected.insert(name, value.clone());
        }

        let from_pairs = Headers::from_pairs(
            pairs.iter().map(|(name, value)| (name.clone(), value.clone())),
        );
        let collected: Headers = pairs
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let borrowed: Vec<(&str, &str)> = pairs
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let from_slice = Headers::from(borrowed.as_slice());

        prop_assert_eq!(&from_pairs, &expected);
        prop_assert_eq!(&collected, &expected);
        prop_assert_eq!(&from_slice, &expected);
    }
}

#[test]
fn empty_header_name_is_preserved() {
    let mut headers = Headers::new();
    headers.insert("", "empty-name");

    assert_eq!(headers.len(), 1);
    assert_eq!(headers.get(""), Some("empty-name"));
    assert!(!headers.is_empty());
}

#[test]
fn duplicate_keys_are_last_wins_without_reordering_other_entries() {
    let mut headers = Headers::new();
    headers.insert("First", "1");
    headers.insert("Second", "2");
    headers.insert("FIRST", "3");

    assert_eq!(headers.len(), 2);
    assert_eq!(headers.get("first"), Some("3"));
    assert_eq!(headers.get("second"), Some("2"));
    assert_eq!(
        headers,
        Headers::from_iter([("first", "3"), ("second", "2")])
    );
}

#[test]
fn large_header_name_is_not_truncated() {
    const NAME_BYTES: usize = 64 * 1024;

    let name = "X".repeat(NAME_BYTES);
    let mut headers = Headers::new();
    headers.insert(&name, "value");

    assert_eq!(headers.len(), 1);
    assert_eq!(headers.get(&name), Some("value"));
    assert_eq!(headers.get(&name.to_ascii_lowercase()), Some("value"));
}

#[test]
fn unicode_names_follow_ascii_only_case_folding() {
    let mut headers = Headers::new();
    headers.insert("Äuthorization", "one");

    assert_eq!(headers.get("Äuthorization"), Some("one"));
    assert_eq!(headers.get("äuthorization"), None);

    headers.insert("äuthorization", "two");
    assert_eq!(headers.len(), 2);
    assert_eq!(headers.get("Äuthorization"), Some("one"));
    assert_eq!(headers.get("äuthorization"), Some("two"));
}

#[test]
fn unicode_header_values_are_preserved_verbatim() {
    let value = "Grüße aus Berlin 🌍\0tail";
    let mut headers = Headers::new();
    headers.insert("X-Greeting", value);

    assert_eq!(headers.get("x-greeting"), Some(value));
    assert_eq!(headers.get("X-GREETING"), Some(value));
}
