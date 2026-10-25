//! The public surface is frozen, so it cannot grow by accident.
//!
//! `tests/api_surface.snapshot` records every public item in `src/` and every
//! `pub use`, sorted. Adding, renaming or removing one fails this test until the
//! snapshot is regenerated on purpose — the reviewer then sees the surface
//! change in the diff instead of discovering it after a release:
//!
//! ```text
//! UPDATE_API_SURFACE=1 cargo test --all-features --test api_freeze
//! ```
//!
//! The same pass enforces the two rules from the 0.1.0 API review: every public
//! enum is `#[non_exhaustive]` (so a new variant stays additive) and no private
//! module leaks through a `pub use`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const SNAPSHOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/api_surface.snapshot");
const SRC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src");

/// What a scan of `src/` found.
#[derive(Default)]
struct Report {
    /// `file:scope:kind name`, one entry per public item.
    items: Vec<String>,
    /// Every `pub enum` with whether it carries `#[non_exhaustive]`.
    enums: Vec<(String, bool)>,
    /// The targets of `pub use`, without the `pub use` token.
    re_exports: Vec<String>,
}

/// Collects every `.rs` file below `dir`.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).expect("src/ is readable");
    for entry in entries {
        let path = entry.expect("directory entry is readable").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// The identifier that follows `prefix`.
fn ident_after<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(prefix)?;
    let end = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    (end > 0).then(|| &rest[..end])
}

/// The type an `impl` block belongs to (`impl Foo for Bar` → `Bar`).
fn impl_target(line: &str) -> Option<String> {
    let head = line.split('{').next()?.trim();
    let after = head.strip_prefix("impl")?.trim();
    let after = match after.strip_prefix('<') {
        // Drop the leading generic parameter list: `impl<T> Foo<T>`.
        Some(rest) => rest.split_once('>').map_or(rest, |(_, tail)| tail).trim(),
        None => after,
    };
    let body = match after.rsplit_once(" for ") {
        Some((_, implementor)) => implementor,
        None => after,
    };
    let name = body
        .trim()
        .trim_start_matches('&')
        .trim_start_matches("mut ")
        .split(['<', ' ', '{'])
        .next()?
        .trim();
    let short = name.rsplit("::").next().unwrap_or(name);
    (!short.is_empty()).then(|| short.to_owned())
}

/// Records one `pub use` in both the item list and the leak list.
fn record_re_export(file: &str, statement: &str, report: &mut Report) {
    let compact = statement.replace([' ', '\n', '\t'], "");
    let target = compact
        .strip_prefix("pubuse")
        .unwrap_or(&compact)
        .trim_end_matches(';');
    report.re_exports.push(format!("{file}:{target}"));
    report.items.push(format!("{file}:use {target}"));
}

/// Scans one file. Lines inside `#[cfg(test)] mod tests` are ignored.
fn scan(source: &str, file: &str, report: &mut Report) {
    let mut attributes: Vec<String> = Vec::new();
    let mut pending_test = false;
    let mut depth: usize = 0;
    let mut hidden_below: Option<usize> = None;
    let mut impl_scope: Option<(String, usize)> = None;
    let mut re_export: Option<String> = None;

    for raw in source.lines() {
        let line = raw.trim();

        if let Some(buffer) = re_export.as_mut() {
            buffer.push('\n');
            buffer.push_str(line);
            if line.contains(';') {
                record_re_export(file, buffer, report);
                re_export = None;
            }
            continue;
        }

        if let Some(floor) = hidden_below {
            if depth < floor {
                hidden_below = None;
            } else {
                depth += line.matches('{').count();
                depth = depth.saturating_sub(line.matches('}').count());
                continue;
            }
        }

        if line.starts_with("#[") {
            pending_test = line == "#[cfg(test)]";
            attributes.push(line.to_owned());
            continue;
        }

        let scope = impl_scope
            .as_ref()
            .map_or_else(String::new, |(name, _)| format!("impl {name}:"));

        if line.starts_with("pub use") {
            if line.contains(';') {
                record_re_export(file, line, report);
            } else {
                re_export = Some(line.to_owned());
            }
        } else if line.starts_with("pub(crate)") || line.starts_with("pub(super)") {
            // Internal by construction: not part of the surface.
        } else {
            let kind_and_name = if let Some(name) =
                ident_after(line, "pub const fn ").or_else(|| ident_after(line, "pub fn "))
            {
                Some(("fn", name))
            } else if let Some(name) = ident_after(line, "pub struct ") {
                Some(("struct", name))
            } else if let Some(name) = ident_after(line, "pub enum ") {
                Some(("enum", name))
            } else if let Some(name) = ident_after(line, "pub trait ") {
                Some(("trait", name))
            } else if let Some(name) = ident_after(line, "pub const ") {
                Some(("const", name))
            } else {
                ident_after(line, "pub mod ").map(|name| ("mod", name))
            };
            let (kind, name) = kind_and_name.unwrap_or(("", ""));
            if !name.is_empty() {
                report.items.push(format!("{file}:{scope}{kind} {name}"));
                if kind == "enum" {
                    let frozen = attributes.iter().any(|attr| attr == "#[non_exhaustive]");
                    report.enums.push((format!("{file}:{name}"), frozen));
                }
            }
        }

        if line.starts_with("impl") && line.contains('{') {
            if let Some(target) = impl_target(line) {
                impl_scope = Some((target, depth + line.matches('{').count()));
            }
        }
        if pending_test && line.starts_with("mod ") {
            hidden_below = Some(depth + line.matches('{').count());
            pending_test = false;
        }

        depth += line.matches('{').count();
        depth = depth.saturating_sub(line.matches('}').count());
        if depth == 0 {
            impl_scope = None;
        }
        attributes.clear();
    }
}

/// The full surface of the crate as `src/` declares it.
fn surface() -> Report {
    let mut files = Vec::new();
    rust_files(Path::new(SRC), &mut files);
    files.sort();
    let mut report = Report::default();
    for path in files {
        let source = fs::read_to_string(&path).expect("source file is readable");
        // Path relative to `src/`, not just the file name: a subdirectory module
        // (`src/matrix_out/mod.rs`) would otherwise be recorded as `src/mod.rs`
        // and collide with the crate root's neighbours.
        let relative = path
            .strip_prefix(SRC)
            .expect("every scanned file lives under src/")
            .to_string_lossy()
            .replace('\\', "/");
        let file = format!("src/{relative}");
        scan(&source, &file, &mut report);
    }
    report.items.sort();
    report.items.dedup();
    report
}

#[test]
fn public_surface_matches_the_snapshot() {
    let report = surface();
    assert!(
        report.items.len() >= 60,
        "the scan found only {} items — the parser, not the crate, is broken",
        report.items.len()
    );

    let rendered = format!("{}\n", report.items.join("\n"));
    if std::env::var("UPDATE_API_SURFACE").is_ok() {
        fs::write(SNAPSHOT, &rendered).expect("snapshot is writable");
        return;
    }

    let expected = fs::read_to_string(SNAPSHOT).expect("tests/api_surface.snapshot exists");
    let expected: BTreeSet<&str> = expected.lines().filter(|l| !l.trim().is_empty()).collect();
    let actual: BTreeSet<&str> = rendered.lines().filter(|l| !l.trim().is_empty()).collect();

    let removed: Vec<&&str> = expected.difference(&actual).collect();
    let added: Vec<&&str> = actual.difference(&expected).collect();
    assert!(
        removed.is_empty() && added.is_empty(),
        "public surface changed — removed {removed:?}, added {added:?}. \
         Regenerate with UPDATE_API_SURFACE=1 cargo test --test api_freeze if the change is intended."
    );
}

#[test]
fn every_public_enum_is_non_exhaustive() {
    let report = surface();
    assert!(
        !report.enums.is_empty(),
        "no public enum found — the scan is broken"
    );
    let exhaustive: Vec<&String> = report
        .enums
        .iter()
        .filter(|(_, frozen)| !frozen)
        .map(|(name, _)| name)
        .collect();
    assert_eq!(
        exhaustive,
        Vec::<&String>::new(),
        "public enums without #[non_exhaustive]: a new variant would be a breaking change"
    );
}

#[test]
fn no_private_module_leaks_through_a_re_export() {
    let report = surface();
    assert!(
        !report.re_exports.is_empty(),
        "no re-export found — the scan is broken"
    );
    let internals = [
        "json::",
        "utf16_len",
        "json_payload",
        "check_body",
        "recipient",
    ];
    for line in &report.re_exports {
        for internal in internals {
            assert!(
                !line.contains(internal),
                "{line} re-exports the internal item {internal}"
            );
        }
    }
}

/// The scan has to skip `pub(crate)`, struct fields and test modules, and has to
/// attribute methods to their `impl` type — otherwise the snapshot would drift
/// for the wrong reason and the gate would be noise.
#[test]
fn the_parser_sees_items_and_ignores_everything_else() {
    let fixture = r"
pub mod outer;
pub use thing::{Alpha, Beta};
#[non_exhaustive]
pub enum Frozen {
    Only,
}
pub enum Exhaustive {
    Only,
}
pub struct Plain {
    pub field: u8,
}
impl Plain {
    pub const fn new() -> Self {
        Self { field: 0 }
    }
    pub(crate) fn internal(&self) {}
}
impl std::fmt::Display for Plain {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    pub fn hidden() {}
}
";
    let mut report = Report::default();
    scan(fixture, "src/fixture.rs", &mut report);

    let items = &report.items;
    assert!(
        items.contains(&"src/fixture.rs:mod outer".to_owned()),
        "module missing: {items:?}"
    );
    assert!(
        items.contains(&"src/fixture.rs:enum Frozen".to_owned()),
        "enum missing: {items:?}"
    );
    assert!(
        items.contains(&"src/fixture.rs:struct Plain".to_owned()),
        "struct missing: {items:?}"
    );
    assert!(
        items.contains(&"src/fixture.rs:impl Plain:fn new".to_owned()),
        "const fn not attributed to its impl: {items:?}"
    );
    assert!(
        !items.iter().any(|item| item.contains("internal")),
        "pub(crate) leaked into the surface: {items:?}"
    );
    assert!(
        !items.iter().any(|item| item.contains("hidden")),
        "a #[cfg(test)] module leaked into the surface: {items:?}"
    );
    assert!(
        !items.iter().any(|item| item.contains("field")),
        "a struct field was counted as an item: {items:?}"
    );

    let frozen: Vec<&(String, bool)> = report
        .enums
        .iter()
        .filter(|(name, _)| name.ends_with("Frozen"))
        .collect();
    let exhaustive: Vec<&(String, bool)> = report
        .enums
        .iter()
        .filter(|(name, _)| name.ends_with("Exhaustive"))
        .collect();
    assert_eq!(
        frozen.len(),
        1,
        "one frozen enum expected: {:?}",
        report.enums
    );
    assert_eq!(
        exhaustive.len(),
        1,
        "one exhaustive enum expected: {:?}",
        report.enums
    );
    assert!(frozen[0].1, "#[non_exhaustive] was not seen");
    assert!(
        !exhaustive[0].1,
        "an exhaustive enum was reported as frozen"
    );
    assert_eq!(report.re_exports.len(), 1, "{:?}", report.re_exports);
}
