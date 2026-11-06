//! Boundary and property tests for the pure `Headers` helpers.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use messenger_webhook::{
    headers::{MAX_HEADER_NAME_BYTES, MAX_HEADER_VALUE_BYTES},
    Error, Headers,
};
use proptest::prelude::*;

fn header_name() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9!#$%&'*+.^_`|~-]{1,64}".prop_map(String::from)
}

fn header_value() -> impl Strategy<Value = String> {
    "[\t -~]{0,128}".prop_map(String::from)
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
        .rev()
        .find(|(key, _)| key == &name.to_ascii_lowercase())
        .map(|(_, value)| value.as_str())
}

fn append_model(entries: &mut Vec<(String, String)>, name: &str, value: &str) {
    entries.push((name.to_ascii_lowercase(), value.to_owned()));
}

fn get_all_model<'a>(entries: &'a [(String, String)], name: &str) -> Vec<&'a str> {
    let name = name.to_ascii_lowercase();
    entries
        .iter()
        .filter_map(|(key, value)| (key == &name).then_some(value.as_str()))
        .collect()
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
            if headers.insert(name, value.clone()).is_err() {
                continue;
            }
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

    /// The constructor forms preserve duplicates instead of normalising them
    /// last-write-wins, including names that only differ by ASCII case.
    #[test]
    fn constructors_preserve_repeated_fields(
        pairs in prop::collection::vec((header_name(), header_value()), 0..24),
    ) {
        let mut expected = Vec::new();
        for (name, value) in &pairs {
            append_model(&mut expected, name, value);
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

        for headers in [&from_pairs, &collected, &from_slice] {
            prop_assert_eq!(headers.len(), expected.len());
            for (name, _) in &pairs {
                prop_assert_eq!(
                    headers.get_all(name).collect::<Vec<_>>(),
                    get_all_model(&expected, name)
                );
            }
        }
    }
}

#[test]
fn empty_header_name_is_rejected_before_storage() {
    let mut headers = Headers::new();
    let error = headers.insert("", "empty-name").unwrap_err();

    assert!(matches!(error, Error::InvalidHeader { .. }));
    assert_eq!(headers.len(), 0);
}

#[test]
fn constructors_preserve_duplicates_and_get_returns_the_last_field() {
    let mut headers = Headers::new();
    headers.insert("First", "1").unwrap();
    headers.insert("Second", "2").unwrap();
    headers.insert("FIRST", "3").unwrap();

    assert_eq!(
        headers,
        Headers::from_iter([("first", "3"), ("second", "2")])
    );

    let repeated = Headers::from_iter([("First", "1"), ("FIRST", "2"), ("Second", "3")]);
    assert_eq!(repeated.len(), 3);
    assert_eq!(repeated.get("first"), Some("2"));
    assert_eq!(repeated.get("second"), Some("3"));
    assert_eq!(repeated.get_all("first").collect::<Vec<_>>(), ["1", "2"]);

    let mut normalised = repeated;
    normalised.insert("first", "4").unwrap();
    assert_eq!(normalised.len(), 2);
    assert_eq!(normalised.get("first"), Some("4"));
    assert_eq!(normalised.get_all("first").collect::<Vec<_>>(), ["4"]);
}

#[test]
fn header_name_boundary_is_enforced() {
    let at_limit = "X".repeat(MAX_HEADER_NAME_BYTES);
    let over_limit = "X".repeat(MAX_HEADER_NAME_BYTES + 1);
    let mut headers = Headers::new();
    headers.insert(&at_limit, "value").unwrap();

    assert_eq!(headers.len(), 1);
    assert_eq!(headers.get(&at_limit), Some("value"));
    assert!(matches!(
        headers.insert(&over_limit, "value"),
        Err(Error::InvalidHeader { .. })
    ));
}

#[test]
fn header_name_token_set_is_enforced() {
    let mut headers = Headers::new();
    for name in ["Äuthorization", "bad name", "bad:name", "bad\nname"] {
        assert!(matches!(
            headers.insert(name, "one"),
            Err(Error::InvalidHeader { .. })
        ));
    }
    assert_eq!(headers.len(), 0);
}

#[test]
fn header_name_folding_is_ascii_only() {
    let headers = Headers::from_iter([("X-\u{dc}ber", "upper"), ("X-\u{fc}ber", "lower")]);

    assert_eq!(headers.get("x-\u{dc}ber"), Some("upper"));
    assert_eq!(headers.get("X-\u{dc}BER"), Some("upper"));
    assert_eq!(headers.get("x-\u{fc}ber"), Some("lower"));
    assert_eq!(headers.get("X-\u{fc}BER"), Some("lower"));
    assert_eq!(
        headers.get_all("x-\u{dc}ber").collect::<Vec<_>>(),
        ["upper"]
    );
    assert_eq!(
        headers.get_all("x-\u{fc}ber").collect::<Vec<_>>(),
        ["lower"]
    );

    let mut appended = Headers::new();
    appended.append("X-\u{dc}ber", "v");
    assert_eq!(appended.get("x-\u{dc}ber"), Some("v"));
    assert_eq!(appended.get("x-\u{fc}ber"), None, "ASCII folding only");
}

#[test]
fn header_value_boundary_is_enforced() {
    let at_limit = "v".repeat(MAX_HEADER_VALUE_BYTES);
    let over_limit = "v".repeat(MAX_HEADER_VALUE_BYTES + 1);
    let mut headers = Headers::new();
    headers.insert("X-Greeting", &at_limit).unwrap();

    assert_eq!(headers.get("x-greeting"), Some(at_limit.as_str()));
    assert!(matches!(
        headers.insert("X-Greeting", &over_limit),
        Err(Error::InvalidHeader { .. })
    ));
}

#[test]
fn header_value_byte_range_is_enforced() {
    let mut headers = Headers::new();
    for value in [
        "line\nbreak",
        "carriage\rreturn",
        "nul\0byte",
        "non-ascii Grüße",
        "emoji 🌍",
    ] {
        assert!(matches!(
            headers.insert("X-Greeting", value),
            Err(Error::InvalidHeader { .. })
        ));
    }

    headers.insert("X-Greeting", "\tvisible ~ ascii").unwrap();
    assert_eq!(headers.get("x-greeting"), Some("\tvisible ~ ascii"));
}
