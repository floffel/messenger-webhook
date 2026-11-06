//! The advisory gate can only audit optional dependencies when cargo-deny is
//! told to resolve all features. Keep the config from silently shrinking.

use std::fs;

fn deny_config() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/deny.toml");
    fs::read_to_string(path).expect("deny.toml is part of the repository")
}

#[test]
fn advisories_cover_the_all_features_graph() {
    let text = deny_config();
    assert!(
        text.lines().any(|line| line == "[graph]"),
        "deny.toml must have a [graph] section so feature-gated dependencies are audited"
    );
    assert!(
        text.lines().any(|line| line == "all-features = true"),
        "deny.toml [graph] must set all-features = true"
    );
    assert!(
        text.lines()
            .any(|line| line == "unused-ignored-advisory = \"deny\""),
        "deny.toml advisories must reject ignores that match no crate"
    );
}
