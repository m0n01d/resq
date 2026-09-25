//! `set decl --before <PATH>` / `--after <PATH>`: insert a NEW declaration next to an anchor,
//! never replacing anything (STEP 2, hand-off checklist).
//!
//! Every mutating test works on its own throwaway scratch file (`scratch_file`), never the shared
//! `tests/fixtures/`. See `src/edit.rs::insert_relative_to` for the implementation this exercises.

use resq::edit::set_decl_at;
use resq::parser;
use std::path::{Path, PathBuf};
use std::process::Command;
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
// Root level
// ---------------------------------------------------------------------------------------------

#[test]
fn before_at_root() {
    let (_dir, file) = scratch_file("Root.res", "let a = 1\nlet b = 2\n");
    set_decl_at(&file, Some("newDecl"), "let newDecl = 0", Some("b"), None)
        .expect("insert before b");
    let after = read(&file);
    let (ia, iz, ib) = (
        after.find("let a = 1").expect("a present"),
        after.find("let newDecl = 0").expect("newDecl present"),
        after.find("let b = 2").expect("b present"),
    );
    assert!(
        ia < iz && iz < ib,
        "expected order a, newDecl, b:\n{after}"
    );
    assert_reparses_clean(&file);
}

#[test]
fn after_at_root() {
    let (_dir, file) = scratch_file("Root.res", "let a = 1\nlet b = 2\n");
    set_decl_at(&file, Some("newDecl"), "let newDecl = 0", None, Some("a"))
        .expect("insert after a");
    let after = read(&file);
    let (ia, iz, ib) = (
        after.find("let a = 1").expect("a present"),
        after.find("let newDecl = 0").expect("newDecl present"),
        after.find("let b = 2").expect("b present"),
    );
    assert!(
        ia < iz && iz < ib,
        "expected order a, newDecl, b:\n{after}"
    );
    assert_reparses_clean(&file);
}

// ---------------------------------------------------------------------------------------------
// Inside a nested module: indentation must match the anchor's, not the file root's.
// ---------------------------------------------------------------------------------------------

#[test]
fn before_inside_module_keeps_indent() {
    let (_dir, file) = scratch_file("Util.res", "module Util = {\n  let x = 1\n  let y = 2\n}\n");
    set_decl_at(&file, Some("Util.z"), "let z = 3", Some("Util.y"), None)
        .expect("insert before Util.y");
    let after = read(&file);
    assert!(
        after.contains("\n  let z = 3\n"),
        "new declaration must carry the module's 2-space indent:\n{after}"
    );
    let (ix, iz, iy) = (
        after.find("let x = 1").expect("x present"),
        after.find("let z = 3").expect("z present"),
        after.find("let y = 2").expect("y present"),
    );
    assert!(ix < iz && iz < iy, "expected order x, z, y:\n{after}");
    assert!(
        after.trim_end().ends_with('}'),
        "closing brace must survive:\n{after}"
    );
    assert_reparses_clean(&file);
}

#[test]
fn after_inside_module_keeps_indent() {
    let (_dir, file) = scratch_file("Util.res", "module Util = {\n  let x = 1\n  let y = 2\n}\n");
    set_decl_at(&file, Some("Util.z"), "let z = 3", None, Some("Util.x"))
        .expect("insert after Util.x");
    let after = read(&file);
    assert!(
        after.contains("\n  let z = 3\n"),
        "new declaration must carry the module's 2-space indent:\n{after}"
    );
    let (ix, iz, iy) = (
        after.find("let x = 1").expect("x present"),
        after.find("let z = 3").expect("z present"),
        after.find("let y = 2").expect("y present"),
    );
    assert!(ix < iz && iz < iy, "expected order x, z, y:\n{after}");
    assert!(
        after.trim_end().ends_with('}'),
        "closing brace must survive:\n{after}"
    );
    assert_reparses_clean(&file);
}

// ---------------------------------------------------------------------------------------------
// A decorator + doc comment above the anchor are the ANCHOR's attachments, not free-floating —
// `--before` must not land between them and the anchor's own `let`.
// ---------------------------------------------------------------------------------------------

#[test]
fn before_anchor_with_decorator_and_doc_comment_keeps_them_on_anchor() {
    let (_dir, file) = scratch_file(
        "Greet.res",
        "let before = 0\n\n/** doc for greet */\n@genType\nlet greet = () => \"hi\"\n",
    );
    set_decl_at(&file, Some("newBefore"), "let newBefore = 1", Some("greet"), None)
        .expect("insert before greet");
    let after = read(&file);
    assert!(
        after.contains("/** doc for greet */\n@genType\nlet greet = () => \"hi\""),
        "greet's doc comment and decorator must stay attached to it, unsplit:\n{after}"
    );
    let inew = after.find("let newBefore = 1").expect("newBefore present");
    let idoc = after.find("/** doc for greet */").expect("doc comment present");
    assert!(
        inew < idoc,
        "newBefore must land BEFORE greet's decorator/doc, not between them and `let`:\n{after}"
    );
    assert_reparses_clean(&file);
}

// ---------------------------------------------------------------------------------------------
// A trailing `//` comment after the anchor travels with it — `--after` must not land inside it.
// ---------------------------------------------------------------------------------------------

#[test]
fn after_anchor_with_trailing_comment_goes_past_it() {
    let (_dir, file) = scratch_file(
        "Greet.res",
        "let greet = () => \"hi\" // trailing note\nlet after = 0\n",
    );
    set_decl_at(&file, Some("newAfter"), "let newAfter = 1", None, Some("greet"))
        .expect("insert after greet");
    let after = read(&file);
    assert!(
        after.contains("let greet = () => \"hi\" // trailing note\n\nlet newAfter = 1"),
        "newAfter must land after greet's whole trailing comment, not inside it:\n{after}"
    );
    let inote = after.find("trailing note").expect("comment present");
    let inew = after.find("let newAfter = 1").expect("newAfter present");
    let iold = after.find("let after = 0").expect("old after present");
    assert!(
        inote < inew && inew < iold,
        "expected order: trailing comment, newAfter, old after:\n{after}"
    );
    assert_reparses_clean(&file);
}

// ---------------------------------------------------------------------------------------------
// Anchor not found / anchor in another module: refuse, and touch nothing.
// ---------------------------------------------------------------------------------------------

#[test]
fn anchor_not_found_is_refused() {
    let (_dir, file) = scratch_file("Root.res", "let a = 1\n");
    let before = read(&file);
    let err = set_decl_at(&file, Some("newDecl"), "let newDecl = 0", Some("nonexistent"), None)
        .expect_err("anchor does not exist");
    let msg = err.to_string();
    assert!(msg.contains("no declaration at"), "{msg}");
    assert!(msg.contains("nonexistent"), "{msg}");
    assert_eq!(read(&file), before, "a failed insert must not touch the file");
}

#[test]
fn anchor_in_another_module_is_refused() {
    let (_dir, file) = scratch_file(
        "Modules.res",
        "module A = {\n  let x = 1\n}\nmodule B = {\n  let y = 2\n}\n",
    );
    let before = read(&file);
    let err = set_decl_at(&file, Some("B.newDecl"), "let newDecl = 0", Some("A.x"), None)
        .expect_err("anchor A.x is not in module B");
    let msg = err.to_string();
    assert!(msg.contains("module `A`"), "{msg}");
    assert!(msg.contains("module `B`"), "{msg}");
    assert!(msg.contains("A.newDecl"), "{msg}");
    assert_eq!(read(&file), before, "a failed insert must not touch the file");
}

// ---------------------------------------------------------------------------------------------
// `--name` already exists in the file: `--before`/`--after` only ADD, they never replace.
// ---------------------------------------------------------------------------------------------

#[test]
fn existing_name_plus_before_is_refused_byte_identical() {
    let (_dir, file) = scratch_file("Root.res", "let a = 1\nlet b = 2\n");
    let before = read(&file);
    let err = set_decl_at(&file, Some("b"), "let b = 99", Some("a"), None).expect_err("b already exists");
    let msg = err.to_string();
    assert!(msg.contains("already exists"), "{msg}");
    assert!(msg.contains("anchor was `a`"), "{msg}");
    assert_eq!(
        read(&file),
        before,
        "a refused set decl must leave the file byte-identical"
    );
}

#[test]
fn existing_name_plus_after_is_refused_byte_identical() {
    let (_dir, file) = scratch_file("Root.res", "let a = 1\nlet b = 2\n");
    let before = read(&file);
    let err = set_decl_at(&file, Some("a"), "let a = 99", None, Some("b")).expect_err("a already exists");
    let msg = err.to_string();
    assert!(msg.contains("already exists"), "{msg}");
    assert!(msg.contains("anchor was `b`"), "{msg}");
    assert_eq!(
        read(&file),
        before,
        "a refused set decl must leave the file byte-identical"
    );
}

// ---------------------------------------------------------------------------------------------
// `--before` and `--after` together: clap's `conflicts_with` only fires through the real CLI
// parser, so this one drives the actual binary rather than calling edit.rs functions in-process.
// ---------------------------------------------------------------------------------------------

#[test]
fn before_and_after_together_is_a_clap_error() {
    let (_dir, file) = scratch_file("Root.res", "let a = 1\nlet b = 2\n");
    let before = read(&file);
    let out = Command::new(env!("CARGO_BIN_EXE_resq"))
        .args([
            "set",
            "decl",
            file.to_str().expect("utf8 path"),
            "--name",
            "c",
            "--content",
            "let c = 3",
            "--before",
            "a",
            "--after",
            "b",
        ])
        .output()
        .expect("run resq binary");
    assert!(
        !out.status.success(),
        "--before and --after together must be rejected by clap"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--before") && stderr.contains("--after"),
        "clap error should name both conflicting flags:\n{stderr}"
    );
    assert_eq!(
        read(&file),
        before,
        "a rejected CLI call must not touch the file"
    );
}
