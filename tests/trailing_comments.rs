//! Trailing comments travel with their declaration.
//!
//! A declaration owns the comments that follow it on its own last line — `let x = 1 // note`
//! followed by more declarations must not have `// note` orphaned by `rm decl`, missed by `get`,
//! or unreachable by `patch`. See `parser::decl_end` / `parser::decl_full_span`.
//!
//! Every mutating test works on its own throwaway scratch file (`scratch_file`), never the shared
//! `tests/fixtures/`.

use resq::cli::{AddOpen, RmOpen};
use resq::edit::{patch, rm_decl, set_decl};
use resq::extract::extract_group;
use resq::imports::{run_add_open, run_rm_open};
use resq::parser;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn scratch_file(name: &str, contents: &str) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(name);
    std::fs::write(&path, contents).expect("write scratch file");
    (dir, path)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).expect("read scratch file")
}

fn assert_reparses_clean(path: &Path) {
    let src = read(path);
    let tree = parser::parse(&src).expect("parse");
    assert!(
        !tree.root_node().has_error(),
        "{} does not re-parse clean:\n{src}",
        path.display()
    );
}

// ---------------------------------------------------------------------------------------------
// The repro from the brief, with made-up names: `type tab = HomeTab | SettingsTab // sync confirmed done; ...`
// ---------------------------------------------------------------------------------------------

const REPRO: &str = "type tab = HomeTab | SettingsTab // sync confirmed done; polling continues until savedAt\nlet other = 1\n";

#[test]
fn get_includes_the_trailing_comment() {
    let (_dir, file) = scratch_file("Tabs.res", REPRO);
    let results = extract_group(&file, &["tab".to_string()]).expect("get tab");
    assert_eq!(results.len(), 1);
    assert!(
        results[0]
            .source
            .contains("// sync confirmed done; polling continues until savedAt"),
        "get did not include the trailing comment:\n{}",
        results[0].source
    );
    assert!(!results[0].source.contains("let other"));
}

#[test]
fn patch_reaches_text_inside_the_trailing_comment() {
    let (_dir, file) = scratch_file("Tabs.res", REPRO);
    patch(&file, "tab", "polling continues", "polling stops")
        .expect("patch should find text inside the trailing comment");
    let after = read(&file);
    assert!(after.contains("polling stops"));
    assert!(!after.contains("polling continues"));
    assert_reparses_clean(&file);
}

#[test]
fn rm_decl_leaves_no_orphan_comment() {
    let (_dir, file) = scratch_file("Tabs.res", REPRO);
    rm_decl(&file, &["tab".to_string()]).expect("rm decl tab");
    let after = read(&file);
    assert!(
        !after.contains("sync confirmed done"),
        "trailing comment was left as an orphan:\n{after}"
    );
    assert_eq!(after.trim(), "let other = 1");
    assert_reparses_clean(&file);
}

#[test]
fn set_decl_replaces_declaration_and_its_trailing_comment() {
    let (_dir, file) = scratch_file("Tabs.res", REPRO);
    set_decl(&file, Some("tab"), "type tab = HomeTab").expect("replace tab");
    let after = read(&file);
    assert!(!after.contains("sync confirmed done"));
    assert!(after.contains("type tab = HomeTab"));
    assert!(after.contains("let other = 1"));
    assert_reparses_clean(&file);
}

// ---------------------------------------------------------------------------------------------
// A comment on the next line is a different thing — free-standing prose, not attached.
// ---------------------------------------------------------------------------------------------

#[test]
fn comment_on_the_next_line_is_not_attached() {
    let src = "let x = 1\n// next line comment, not attached to x\nlet y = 2\n";
    let (_dir, file) = scratch_file("Next.res", src);
    let results = extract_group(&file, &["x".to_string()]).expect("get x");
    assert_eq!(results[0].source, "let x = 1");

    rm_decl(&file, &["x".to_string()]).expect("rm decl x");
    let after = read(&file);
    assert!(
        after.contains("// next line comment, not attached to x"),
        "a comment on the next line must survive removal of the earlier declaration:\n{after}"
    );
    assert_reparses_clean(&file);
}

// ---------------------------------------------------------------------------------------------
// A `/** doc */` immediately after a declaration is the *next* declaration's leading attachment,
// never the previous one's trailing comment — the two spans must never overlap.
// ---------------------------------------------------------------------------------------------

#[test]
fn doc_comment_for_next_decl_is_not_taken_as_trailing() {
    let src = "let a = 1 /** doc for b */\nlet b = 2\n";
    let (_dir, file) = scratch_file("DocNext.res", src);

    let a = extract_group(&file, &["a".to_string()]).expect("get a");
    assert_eq!(a[0].source, "let a = 1");
    let b = extract_group(&file, &["b".to_string()]).expect("get b");
    assert!(b[0].source.contains("/** doc for b */"));
    assert!(b[0].source.contains("let b = 2"));

    rm_decl(&file, &["a".to_string()]).expect("rm decl a");
    let after = read(&file);
    assert!(
        after.contains("/** doc for b */"),
        "b's doc comment must survive removal of a:\n{after}"
    );
    assert!(after.contains("let b = 2"));
    assert_reparses_clean(&file);
}

// ---------------------------------------------------------------------------------------------
// Nested module: the trailing comment travels with the member, and the block's closing `}` is
// never touched.
// ---------------------------------------------------------------------------------------------

#[test]
fn nested_module_trailing_comment_leaves_closing_brace_untouched() {
    let src = "module M = {\n  let x = 1 // one\n}\n";
    let (_dir, file) = scratch_file("Nested.res", src);

    let results = extract_group(&file, &["M.x".to_string()]).expect("get M.x");
    assert!(results[0].source.contains("// one"));

    rm_decl(&file, &["M.x".to_string()]).expect("rm decl M.x");
    let after = read(&file);
    assert!(!after.contains("// one"));
    assert!(
        after.contains('}'),
        "the module's closing brace must survive:\n{after}"
    );
    assert_reparses_clean(&file);
}

// ---------------------------------------------------------------------------------------------
// `rm open` has the same shape of bug as `rm decl`: `imports.rs::removal_span` used
// `node.end_byte()` directly rather than `parser::decl_end`, so a trailing `//` comment on the
// `open`'s own line was left behind as an orphan instead of being removed with it.
// ---------------------------------------------------------------------------------------------

#[test]
fn rm_open_leaves_no_orphan_trailing_comment() {
    let (_dir, file) = scratch_file("Imports.res", "open Belt // for Array\nlet x = 1\n");
    run_rm_open(RmOpen {
        file: file.clone(),
        modules: vec!["Belt".to_string()],
        force: false,
    })
    .expect("rm open should succeed: nothing in this file has an unqualified reference");
    let after = read(&file);
    assert!(
        !after.contains("for Array"),
        "the open's trailing comment was left as an orphan:\n{after}"
    );
    assert_eq!(after, "let x = 1\n");
    assert_reparses_clean(&file);
}

// ---------------------------------------------------------------------------------------------
// `add open` / `add alias` have the same shape of bug as `rm open`: `imports.rs::insertion_offset`
// used to search for the next `\n` starting at the anchor's own `end_byte()`, which sits BEFORE a
// trailing same-line block comment. With `open Belt /* start\nend */`, that search finds the `\n`
// *inside* the comment (between "start" and "end") and splices the new line there — the file still
// parses (comments are opaque to the grammar), but the new `open`/alias never takes effect.
// ---------------------------------------------------------------------------------------------

#[test]
fn add_open_lands_after_a_trailing_block_comment_not_inside_it() {
    let (_dir, file) = scratch_file("Imports.res", "open Belt /* start\nend */\n\nlet x = 1\n");
    run_add_open(AddOpen {
        file: file.clone(),
        modules: vec!["Js".to_string()],
    })
    .expect("add open should succeed");
    let after = read(&file);
    assert!(
        after.contains("end */\nopen Js"),
        "the new open must land after the block comment closes, not inside it:\n{after}"
    );
    assert!(
        !after.contains("start\nopen Js"),
        "the new open must not land inside the block comment:\n{after}"
    );
    assert_reparses_clean(&file);

    // The bug's real-world symptom: the new open silently never took effect.
    let tree = parser::parse(&after).expect("parse");
    let opens = resq::analysis::extract_summary(&tree, &after, "Imports").opens;
    assert!(
        opens.iter().any(|o| o == "Js"),
        "`open Js` must be a real open, not text trapped inside a comment: {opens:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// `/* a */ // b` — both ordinary comments trail the declaration and are taken together.
// ---------------------------------------------------------------------------------------------

#[test]
fn block_then_line_comment_both_taken() {
    let src = "let x = 1 /* a */ // b\nlet y = 2\n";
    let (_dir, file) = scratch_file("Both.res", src);

    let results = extract_group(&file, &["x".to_string()]).expect("get x");
    assert!(results[0].source.contains("/* a */"));
    assert!(results[0].source.contains("// b"));

    rm_decl(&file, &["x".to_string()]).expect("rm decl x");
    let after = read(&file);
    assert!(!after.contains("/* a */"));
    assert!(!after.contains("// b"));
    assert!(after.contains("let y = 2"));
    assert_reparses_clean(&file);
}

// ---------------------------------------------------------------------------------------------
// A `;` after the binding is not a comment, but it sits on the declaration's own last row and
// must be absorbed by `decl_end` the same way a trailing comment is — otherwise `get` leaves the
// comment out, and `rm decl` leaves `; // note` (or a bare `;`) behind as an orphan.
// ---------------------------------------------------------------------------------------------

#[test]
fn get_includes_the_semicolon_and_trailing_comment() {
    let src = "let x = 1; // note\nlet y = 2\n";
    let (_dir, file) = scratch_file("Semi.res", src);
    let results = extract_group(&file, &["x".to_string()]).expect("get x");
    assert_eq!(results[0].source, "let x = 1; // note");
}

#[test]
fn rm_decl_removes_semicolon_and_trailing_comment() {
    let src = "let x = 1; // note\nlet y = 2\n";
    let (_dir, file) = scratch_file("Semi.res", src);
    rm_decl(&file, &["x".to_string()]).expect("rm decl x");
    let after = read(&file);
    assert!(
        !after.contains(';') && !after.contains("note"),
        "the semicolon and its trailing comment must not be left behind:\n{after}"
    );
    assert_eq!(after.trim(), "let y = 2");
    assert_reparses_clean(&file);
}

#[test]
fn rm_decl_removes_bare_semicolon_with_no_comment() {
    let src = "let x = 1;\nlet y = 2\n";
    let (_dir, file) = scratch_file("SemiOnly.res", src);
    rm_decl(&file, &["x".to_string()]).expect("rm decl x");
    let after = read(&file);
    assert!(
        !after.contains(';'),
        "the bare semicolon must not be left behind:\n{after}"
    );
    assert_eq!(after.trim(), "let y = 2");
    assert_reparses_clean(&file);
}
