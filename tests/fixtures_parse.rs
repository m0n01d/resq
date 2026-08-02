//! Conductor-owned harness test: every fixture must parse as expected.
use std::fs;

fn parser() -> tree_sitter::Parser {
    let mut p = tree_sitter::Parser::new();
    p.set_language(&tree_sitter_rescript::LANGUAGE.into()).unwrap();
    p
}

#[test]
fn fixtures_parse_as_expected() {
    let mut p = parser();
    let mut checked = 0;
    for e in walkdir::WalkDir::new("tests/fixtures").into_iter().filter_map(|e| e.ok()) {
        let path = e.path();
        if !path.extension().is_some_and(|x| x == "res" || x == "resi") {
            continue;
        }
        let src = fs::read_to_string(path).unwrap();
        let tree = p.parse(&src, None).unwrap();
        let expect_err = path.to_string_lossy().contains("broken");
        assert_eq!(
            tree.root_node().has_error(),
            expect_err,
            "unexpected parse result for {}",
            path.display()
        );
        checked += 1;
    }
    assert!(checked >= 6, "expected at least 6 fixture files, found {checked}");
}
