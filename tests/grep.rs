//! Wave 2 (agent A5) — `resq grep`: regex search annotated with the enclosing declaration.
//!
//! Exercises `resq::grep::{search, execute, GrepArgs}` directly against `tests/fixtures/proj/`,
//! per the acceptance list in the dispatch prompt. `search` is the pure, side-effect-free seam
//! (no printing) so matches and their annotated dot-paths can be asserted on directly; `execute`
//! is exercised separately for the process-exit-code contract (0/1/2).

use resq::DeclarationKind;
use resq::cli::Format;
use resq::grep::{GrepArgs, execute, search};
use std::path::PathBuf;

const MAIN: &str = "tests/fixtures/proj/src/Main.res";

fn args(pattern: &str) -> GrepArgs {
    GrepArgs {
        pattern: pattern.to_string(),
        path: Some(PathBuf::from("tests/fixtures/proj")),
        fixed: false,
        ignore_case: false,
        include_comments: false,
        include_strings: false,
        definitions: false,
        source: false,
        format: Format::Compact,
    }
}

// -------------------------------------------------------------------------------------------
// 1. A match is annotated with the correct enclosing dot-path — including a nested case.
// -------------------------------------------------------------------------------------------

#[test]
fn nested_match_in_inner_deep_gets_the_full_dot_path() {
    // `42` occurs exactly once in Main.res, inside `module Inner = { module Deep = { let
    // deepValue = 42 } }`. The annotation must reach two levels of nesting.
    let hits = search(&args("42")).unwrap();
    let hit = hits
        .iter()
        .find(|h| h.file.ends_with("Main.res"))
        .expect("a match for `42` in Main.res");

    assert_eq!(
        hit.path.as_ref().map(|p| p.to_string()).as_deref(),
        Some("Inner.Deep.deepValue"),
        "must resolve through two levels of module nesting"
    );
    assert_eq!(hit.kind, Some(DeclarationKind::Let));
}

#[test]
fn match_in_function_body_gets_its_enclosing_declaration() {
    // `Increment` appears inside `update`'s `switch` body — not itself a declaration, so the
    // match must resolve to the enclosing `let update = …`, not to a bogus inner name.
    let hits = search(&args("Increment")).unwrap();
    let hit = hits
        .iter()
        .find(|h| h.file.ends_with("Main.res"))
        .expect("a match for `Increment` in Main.res");
    assert_eq!(hit.path.as_ref().unwrap().to_string(), "update");
    assert_eq!(hit.kind, Some(DeclarationKind::Let));
}

#[test]
fn nested_module_declaration_walk_matches_the_fixture_shape() {
    // A synthetic two-level nesting, independent of Main.res, as a second confirmation that
    // nesting depth isn't hard-coded to exactly one level.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Nested.res");
    std::fs::write(
        &file,
        "module A = {\n  module B = {\n    module C = {\n      let leaf = 99\n    }\n  }\n}\n",
    )
    .unwrap();

    let mut a = args("99");
    a.path = Some(file);
    let hits = search(&a).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path.as_ref().unwrap().to_string(), "A.B.C.leaf");
}

// -------------------------------------------------------------------------------------------
// 2. Comment-only pattern: excluded by default, included with --include-comments.
// -------------------------------------------------------------------------------------------

#[test]
fn comment_only_match_is_excluded_by_default_and_included_with_the_flag() {
    // "Top-level entry point." appears only inside `entry`'s `/** … */` doc comment.
    let hits = search(&args("Top-level entry point")).unwrap();
    assert!(
        hits.is_empty(),
        "a doc-comment match must be excluded by default, found {} hit(s)",
        hits.len()
    );

    let mut with_flag = args("Top-level entry point");
    with_flag.include_comments = true;
    let hits = search(&with_flag).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path.as_ref().unwrap().to_string(), "entry");
}

// -------------------------------------------------------------------------------------------
// 3. String-only pattern (unicode): excluded by default, included with --include-strings, with
//    UTF-8-safe line/column reporting.
// -------------------------------------------------------------------------------------------

#[test]
fn unicode_string_match_is_excluded_by_default_and_utf8_safe_with_the_flag() {
    // Main.res: `let unicodeString = "héllo — wörld ✓ 日本語"`. "wörld" sits after several
    // multi-byte characters on the line — the classic byte-vs-char column bug.
    let hits = search(&args("wörld")).unwrap();
    assert!(hits.is_empty(), "a string-literal match must be excluded by default");

    let mut with_flag = args("wörld");
    with_flag.include_strings = true;
    let hits = search(&with_flag).unwrap();
    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    assert_eq!(hit.path.as_ref().unwrap().to_string(), "unicodeString");

    // Cross-check line/column against the already-verified parser helper directly, rather than
    // hardcoding a magic column number.
    let source = std::fs::read_to_string(MAIN).unwrap();
    let offset = source.find("wörld").unwrap();
    let (line, col) = resq::parser::byte_offset_to_line_col(&source, offset);
    assert_eq!((hit.line, hit.column), (line, col));
    assert_eq!(hit.line, 29);
}

// -------------------------------------------------------------------------------------------
// 4. Exit code is 1 (not 0, not an error) when there are no matches.
// -------------------------------------------------------------------------------------------

#[test]
fn exit_code_is_one_when_there_are_no_matches() {
    let code = execute(args("ThisPatternDoesNotExistAnywhereInTheFixtures12345"));
    assert_eq!(code, 1);
}

#[test]
fn exit_code_is_zero_when_matches_are_found() {
    let code = execute(args("entry"));
    assert_eq!(code, 0);
}

#[test]
fn exit_code_is_two_on_an_invalid_regex() {
    let code = execute(args("[unclosed"));
    assert_eq!(code, 2);
}

// -------------------------------------------------------------------------------------------
// 5. -F treats the pattern literally.
// -------------------------------------------------------------------------------------------

#[test]
fn fixed_flag_matches_a_real_arrow_chain_literally() {
    // Modern.res: `xs->Array.map(x => x * 2)->Array.filter(x => x > 2)`.
    let mut a = args("->Array.map");
    a.fixed = true;
    let hits = search(&a).unwrap();
    assert!(
        hits.iter().any(|h| h.file.ends_with("Modern.res")),
        "-F must match the literal `->Array.map` substring"
    );
}

#[test]
fn fixed_flag_does_not_treat_dot_as_a_wildcard() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Lit.res");
    // One line has the literal three characters `a.b`; the other has `axb`. A wildcard `.`
    // matches both; a fixed (literal) `.` matches only the first.
    std::fs::write(&file, "let dotted = a.b\nlet crossed = axb\n").unwrap();

    let mut fixed = GrepArgs {
        pattern: "a.b".to_string(),
        path: Some(file.clone()),
        fixed: true,
        ignore_case: false,
        include_comments: false,
        include_strings: false,
        definitions: false,
        source: false,
        format: Format::Compact,
    };
    let hits = search(&fixed).unwrap();
    assert_eq!(hits.len(), 1, "fixed `a.b` must match only the literal text, not `axb`");

    fixed.fixed = false;
    let hits = search(&fixed).unwrap();
    assert_eq!(
        hits.len(),
        2,
        "without -F, `.` is a wildcard and must match both `a.b` and `axb`"
    );
}

// -------------------------------------------------------------------------------------------
// 6. A directory containing an unparseable file still returns matches from its siblings.
// -------------------------------------------------------------------------------------------

#[test]
fn unparseable_sibling_file_does_not_block_the_rest_of_the_search() {
    // tests/fixtures/ contains both `broken.res` (a deliberately-broken fixture with an ERROR
    // node) and `proj/`, which parses cleanly. The broken file must not abort the whole walk.
    let mut a = args("entry");
    a.path = Some(PathBuf::from("tests/fixtures"));
    let hits = search(&a).unwrap();
    assert!(
        hits.iter().any(|h| h.file.ends_with("Main.res")),
        "matches from good sibling files must still surface"
    );
    assert!(
        !hits.iter().any(|h| h.file.ends_with("broken.res")),
        "broken.res has no `entry` text of its own"
    );

    assert_eq!(execute(a), 0, "the run as a whole must still report success");
}

// -------------------------------------------------------------------------------------------
// Extra coverage: --definitions restricts matches to declaration names.
// -------------------------------------------------------------------------------------------

#[test]
fn definitions_flag_excludes_call_sites_and_keeps_the_declaration_name() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Def.res");
    std::fs::write(&file, "let helper = x => x + 1\nlet other = helper(2)\n").unwrap();

    let mut a = GrepArgs {
        pattern: "helper".to_string(),
        path: Some(file),
        fixed: false,
        ignore_case: false,
        include_comments: false,
        include_strings: false,
        definitions: true,
        source: false,
        format: Format::Compact,
    };
    let hits = search(&a).unwrap();
    assert_eq!(
        hits.len(),
        1,
        "only the declaration's own name should match, not the call site on line 2"
    );
    assert_eq!(hits[0].line, 1);

    a.definitions = false;
    let hits = search(&a).unwrap();
    assert_eq!(hits.len(), 2, "without --definitions both occurrences match");
}

// -------------------------------------------------------------------------------------------
// Smoke tests: --source and --format json don't panic and still honor the exit-code contract.
// -------------------------------------------------------------------------------------------

#[test]
fn source_mode_runs_cleanly() {
    let mut a = args("deepValue");
    a.source = true;
    assert_eq!(execute(a), 0);
}

#[test]
fn json_format_runs_cleanly() {
    let mut a = args("entry");
    a.format = Format::Json;
    assert_eq!(execute(a), 0);
}

#[test]
fn single_file_path_argument_is_searched_directly() {
    let mut a = args("entry");
    a.path = Some(PathBuf::from(MAIN));
    let hits = search(&a).unwrap();
    assert!(!hits.is_empty());
    assert!(hits.iter().all(|h| h.file.ends_with("Main.res")));
}
