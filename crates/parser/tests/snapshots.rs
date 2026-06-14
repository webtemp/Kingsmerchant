//! Snapshot tests against real game-copied item strings (PRD §4.3, §7).
//!
//! Each `tests/items/*.txt` fixture is a verbatim POE2 clipboard capture. We
//! parse it and snapshot the resulting struct; `cargo insta review` shows any
//! diff when the parser's output changes.

use parser::parse_item;

#[test]
fn parses_real_item_fixtures() {
    insta::glob!("items/*.txt", |path| {
        let input = std::fs::read_to_string(path).unwrap();
        let item = parse_item(&input)
            .unwrap_or_else(|e| panic!("failed to parse {}: {e}", path.display()));
        insta::assert_debug_snapshot!(item);
    });
}

/// POE2 emits both a hyphen and an em dash as the descriptor tag separator
/// (the same helmet copied twice produced each). They must parse identically.
#[test]
fn hyphen_and_emdash_descriptors_are_equivalent() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/items/");
    let read = |name: &str| std::fs::read_to_string(format!("{dir}{name}")).unwrap();
    let hyphen = parse_item(&read("rare_helmet.txt")).unwrap();
    let emdash = parse_item(&read("rare_helmet_emdash.txt")).unwrap();
    assert_eq!(hyphen, emdash);
}
