//! Wave 2 (agent A3) — tests for `src/extract.rs`, the `resq get` command.
//!
//! Exercises `tests/fixtures/proj/` per the task's required cases:
//! 1. `View.res make` carries `@react.component` and the full JSX body.
//! 2. `Main.res entry` carries both the doc comment and `@genType`.
//! 3. `Main.res Inner.Deep.deepValue` resolves a nested dot-path.
//! 4. `Main.res deepValue` (bare name for a nested decl) fails — no implicit search.
//! 5. A missing path exits non-zero (returns `Err`).
//! 6. `Modern.res even` works — one binding of a multi-binding `let rec … and` declaration.

use resq::cli::Format;
use resq::cli::{Cli, Command};
use resq::extract::{GetResult, extract_group, extract_many, run};
use clap::Parser;
use std::path::{Path, PathBuf};

const MAIN: &str = "tests/fixtures/proj/src/Main.res";
const VIEW: &str = "tests/fixtures/proj/src/View.res";
const MODERN: &str = "tests/fixtures/proj/src/Modern.res";

fn get_one(file: &str, path: &str) -> GetResult {
    extract_group(Path::new(file), &[path.to_string()])
        .unwrap_or_else(|e| panic!("get {file} {path} failed: {e}"))
        .into_iter()
        .next()
        .unwrap()
}

// -------------------------------------------------------------------------------------------
// Required case 1: View.res `make` — @react.component and the full JSX body.
// -------------------------------------------------------------------------------------------

#[test]
fn view_make_carries_react_component_and_jsx_body() {
    let result = get_one(VIEW, "make");
    assert!(
        result.source.contains("@react.component"),
        "decorator missing from get output: {:?}",
        result.source
    );
    assert!(result.source.starts_with("@react.component"));
    assert!(result.source.contains("let make = (~name: string, ~count: int) => {"));
    assert!(result.source.contains(r#"<div className="wrap">"#));
    assert!(result.source.contains("{React.string(name)}"));
    assert!(result.source.contains("{React.int(count)}"));
    assert!(result.source.trim_end().ends_with('}'));
    assert_eq!(result.decorators, vec!["@react.component".to_string()]);
    assert_eq!(result.kind, resq::DeclarationKind::Let);
    assert_eq!(result.path, "make");
    assert_eq!(result.file, VIEW);
}

// -------------------------------------------------------------------------------------------
// Required case 2: Main.res `entry` — doc comment AND @genType both present.
// -------------------------------------------------------------------------------------------

#[test]
fn main_entry_carries_doc_comment_and_gentype() {
    let result = get_one(MAIN, "entry");
    assert!(
        result.source.contains("/** Top-level entry point. */"),
        "doc comment missing: {:?}",
        result.source
    );
    assert!(
        result.source.contains("@genType"),
        "decorator missing: {:?}",
        result.source
    );
    assert!(result.source.contains("let entry = () => 1"));
    assert_eq!(
        result.doc_comment.as_deref(),
        Some("/** Top-level entry point. */")
    );
    assert_eq!(result.decorators, vec!["@genType".to_string()]);
    assert_eq!(result.start_line, 4);
    assert_eq!(result.end_line, 6);
}

// -------------------------------------------------------------------------------------------
// Required case 3: Main.res `Inner.Deep.deepValue` — nested dot-path resolution.
// -------------------------------------------------------------------------------------------

#[test]
fn main_nested_dot_path_resolves() {
    let result = get_one(MAIN, "Inner.Deep.deepValue");
    assert_eq!(result.source, "let deepValue = 42");
    assert_eq!(result.path, "Inner.Deep.deepValue");
    assert_eq!(result.kind, resq::DeclarationKind::Let);
}

/// Also check a one-deep nested path and its doc comment, since `Inner.helper` has one and it must
/// not bleed onto `Inner.Deep.deepValue`.
#[test]
fn main_one_deep_nested_path_resolves_with_its_own_doc_comment() {
    let result = get_one(MAIN, "Inner.helper");
    assert!(result.source.contains("/** Nested helper. */"));
    assert!(result.source.contains("let helper = x => x * 2"));
    assert_eq!(result.doc_comment.as_deref(), Some("/** Nested helper. */"));
}

// -------------------------------------------------------------------------------------------
// Required case 4: Main.res `deepValue` (bare name for a nested decl) — must FAIL.
// -------------------------------------------------------------------------------------------

#[test]
fn bare_name_does_not_implicitly_find_nested_declaration() {
    let err = extract_group(Path::new(MAIN), &["deepValue".to_string()])
        .expect_err("bare `deepValue` must not resolve to Inner.Deep.deepValue");
    let msg = err.to_string();
    assert!(msg.contains("deepValue"), "error should name the path: {msg}");
    assert!(msg.contains(MAIN), "error should name the file: {msg}");
}

/// Same check one level up: `helper` bare must not match `Inner.helper`.
#[test]
fn bare_name_does_not_match_one_deep_nested_declaration_either() {
    extract_group(Path::new(MAIN), &["helper".to_string()])
        .expect_err("bare `helper` must not resolve to Inner.helper");
}

// -------------------------------------------------------------------------------------------
// Required case 5: a missing path exits non-zero (returns Err), naming path and file.
// -------------------------------------------------------------------------------------------

#[test]
fn missing_path_is_an_error_naming_path_and_file() {
    let err = extract_group(Path::new(MAIN), &["NoSuchDeclaration".to_string()])
        .expect_err("nonexistent path must error");
    let msg = err.to_string();
    assert!(msg.contains("NoSuchDeclaration"), "error should name the path: {msg}");
    assert!(msg.contains(MAIN), "error should name the file: {msg}");
}

#[test]
fn missing_file_is_an_error() {
    extract_group(Path::new("tests/fixtures/proj/src/DoesNotExist.res"), &["x".to_string()])
        .expect_err("nonexistent file must error");
}

// -------------------------------------------------------------------------------------------
// Required case 6: Modern.res `even` — one binding of a multi-binding `let rec … and` decl
// (SPEC §1 finding 6). Extracting `even` returns the whole `let rec … and …` declaration,
// since `even` and `odd` are mutually recursive and the pair is what the parser models as one
// `Declaration` — that is a correct reflection of the grammar, not a bug.
// -------------------------------------------------------------------------------------------

#[test]
fn modern_even_resolves_multi_binding_let_rec_and() {
    let result = get_one(MODERN, "even");
    assert!(result.source.contains("let rec even = x => x == 0 || odd(x - 1)"));
    assert!(result.source.contains("and odd = x => x != 0 && even(x - 1)"));
    assert_eq!(result.kind, resq::DeclarationKind::Let);
}

/// `odd` is bound by the same declaration and must resolve identically (same span).
#[test]
fn modern_odd_resolves_to_the_same_declaration_as_even() {
    let even = get_one(MODERN, "even");
    let odd = get_one(MODERN, "odd");
    assert_eq!(even.source, odd.source);
    assert_eq!(even.start_line, odd.start_line);
    assert_eq!(even.end_line, odd.end_line);
}

// -------------------------------------------------------------------------------------------
// Ambiguity: requesting a path with several matching declarations must error, not guess.
// (`let (first, second) = (1, 2)` in Main.res binds two names from one destructuring binding —
// that's not ambiguity, it's SPEC §3.7. There is no ambiguous fixture path by construction, so
// this test constructs one from a temp file with genuine shadowing at the same path.)
// -------------------------------------------------------------------------------------------

#[test]
fn ambiguous_path_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Shadow.res");
    std::fs::write(&file, "let dup = 1\nlet dup = 2\n").unwrap();
    let err = extract_group(&file, &["dup".to_string()]).expect_err("shadowed name must be ambiguous");
    assert!(err.to_string().contains("ambiguous"), "error: {err}");
}

// -------------------------------------------------------------------------------------------
// Destructuring binding: `get` on either bound name returns the whole binding (SPEC §3.7).
// -------------------------------------------------------------------------------------------

#[test]
fn destructuring_binding_returns_whole_binding_for_either_name() {
    let first = get_one(MAIN, "first");
    let second = get_one(MAIN, "second");
    assert_eq!(first.source, "let (first, second) = (1, 2)");
    assert_eq!(first.source, second.source);
    assert_eq!(first.start_line, second.start_line);
    assert_eq!(first.end_line, second.end_line);
}

// -------------------------------------------------------------------------------------------
// extract_many / grouped multi-file extraction, in requested order.
// -------------------------------------------------------------------------------------------

#[test]
fn extract_many_preserves_group_and_path_order() {
    let groups = vec![
        (PathBuf::from(VIEW), vec!["polyColor".to_string(), "make".to_string()]),
        (PathBuf::from(MAIN), vec!["entry".to_string()]),
    ];
    let results = extract_many(&groups).unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].path, "polyColor");
    assert_eq!(results[0].file, VIEW);
    assert_eq!(results[1].path, "make");
    assert_eq!(results[1].file, VIEW);
    assert_eq!(results[2].path, "entry");
    assert_eq!(results[2].file, MAIN);
}

// -------------------------------------------------------------------------------------------
// CLI shape: clap flattens repeated `-f FILE PATH...` occurrences into one `Vec<String>` with no
// group boundary preserved in the typed field (`Command::Get.from`). extract::run must recover the
// grouping by recognizing file-shaped tokens (`.res`/`.resi` suffix or a path separator). This
// verifies real clap parsing behaviour end-to-end, not just our assumption about it.
// -------------------------------------------------------------------------------------------

#[test]
fn cli_grouped_form_regroups_flattened_from_vec() {
    let cli = Cli::parse_from([
        "resq",
        "get",
        "-f",
        MAIN,
        "entry",
        "-f",
        VIEW,
        "make",
        "polyColor",
    ]);
    let Command::Get { file, names, from, format: _ } = cli.command else {
        panic!("expected Command::Get");
    };
    assert_eq!(file, None, "bare positional file must be absent in grouped form");
    assert!(names.is_empty(), "bare positional names must be absent in grouped form");
    // clap flattens both occurrences into one Vec<String> — confirm that shape empirically rather
    // than assuming it, then confirm `run` still produces the right grouping end-to-end.
    assert_eq!(
        from,
        vec![
            MAIN.to_string(),
            "entry".to_string(),
            VIEW.to_string(),
            "make".to_string(),
            "polyColor".to_string(),
        ]
    );

    run(file, names, from, Format::Compact).expect("grouped get should succeed");
}

#[test]
fn cli_bare_form_parses_file_and_names() {
    let cli = Cli::parse_from(["resq", "get", MAIN, "entry", "first"]);
    let Command::Get { file, names, from, format: _ } = cli.command else {
        panic!("expected Command::Get");
    };
    assert_eq!(file, Some(PathBuf::from(MAIN)));
    assert_eq!(names, vec!["entry".to_string(), "first".to_string()]);
    assert!(from.is_empty());
}

// -------------------------------------------------------------------------------------------
// .resi files parse with the same nodes (SPEC §1 finding 5) and `get` works on them for free.
// -------------------------------------------------------------------------------------------

#[test]
fn get_works_on_resi_signature_file() {
    let result = get_one("tests/fixtures/proj/src/View.resi", "make");
    assert!(result.source.contains("@react.component"));
    assert!(result.source.contains("let make: (~name: string, ~count: int) => React.element"));
}
