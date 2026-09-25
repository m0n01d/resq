//! Tests for anonymous top-level binding addressing: `let () = …` and `let _ = …`.
//!
//! Before this fix, a `let` binding whose whole pattern is `()` or `_` bound zero names
//! (`parser::let_declaration_parts` -> `bound_names` -> `bound_name_spans` skips `_` and has no
//! case for the unit pattern), so `resq list` showed it as a bare, nameless line and no command
//! could address it. The fix gives the whole-pattern `()` the literal name `"()"` and the
//! whole-pattern `_` the literal name `"_"`, so every command addresses them like any other name.
//!
//! Every mutating test writes to its own `tempfile::TempDir`, never to `tests/fixtures/`.

use resq::analysis::extract_summary;
use resq::edit::{patch, rm_decl, set_decl, set_decl_at};
use resq::extract::extract_group;
use resq::parser::parse;
use resq::refs::find;
use resq::BinderKind;

// -------------------------------------------------------------------------------------------
// Scratch helpers
// -------------------------------------------------------------------------------------------

/// A throwaway `.res` file holding `contents`, inside its own tempdir (kept alive by the return
/// value so the path stays valid for the test's duration).
fn scratch_res(contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("AllTests.res");
    std::fs::write(&path, contents).expect("write scratch file");
    (dir, path)
}

/// A throwaway ReScript project (`rescript.json` + `src/`), for the commands (`refs`) that need a
/// project root to resolve.
fn scratch_project(files: &[(&str, &str)]) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("rescript.json"),
        r#"{ "name": "tmp-proj", "sources": [{ "dir": "src", "subdirs": true }] }"#,
    )
    .expect("write rescript.json");
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).expect("create src dir");
    for (name, contents) in files {
        std::fs::write(src.join(name), contents).expect("write project file");
    }
    (dir, src)
}

// -------------------------------------------------------------------------------------------
// list: `()` and `_` show up as real names, with `binder_kind` still `Simple`.
// -------------------------------------------------------------------------------------------

#[test]
fn list_shows_unit_and_wildcard_as_names() {
    let (_dir, path) = scratch_res(
        "let () = {\n  Console.log(\"test 1\")\n}\n\nlet _ = Console.log(\"side effect\")\n",
    );
    let src = std::fs::read_to_string(&path).unwrap();
    let tree = parse(&src).expect("parses");
    let summary = extract_summary(&tree, &src, "AllTests");

    let unit = summary
        .declarations
        .iter()
        .find(|d| d.names == vec!["()".to_string()])
        .expect("`let ()` should be named `()`");
    assert_eq!(unit.binder_kind, BinderKind::Simple);

    let wildcard = summary
        .declarations
        .iter()
        .find(|d| d.names == vec!["_".to_string()])
        .expect("`let _` should be named `_`");
    assert_eq!(wildcard.binder_kind, BinderKind::Simple);
}

/// A type annotation on either side does not change the name resq assigns.
#[test]
fn type_annotation_does_not_change_the_name() {
    let (_dir, path) = scratch_res("let (): unit = ()\nlet _: int = 1\n");
    let src = std::fs::read_to_string(&path).unwrap();
    let tree = parse(&src).expect("parses");
    let summary = extract_summary(&tree, &src, "AllTests");
    assert!(summary.declarations.iter().any(|d| d.names == vec!["()".to_string()]));
    assert!(summary.declarations.iter().any(|d| d.names == vec!["_".to_string()]));
}

/// Other nameless patterns stay unaddressable: `_` (and any other pattern) nested *inside* a
/// destructuring binding is not the whole pattern, so it is not promoted to a name.
#[test]
fn nested_wildcard_inside_a_tuple_stays_unaddressable() {
    let (_dir, path) = scratch_res("let (_, _) = (1, 2)\n");
    let src = std::fs::read_to_string(&path).unwrap();
    let tree = parse(&src).expect("parses");
    let summary = extract_summary(&tree, &src, "AllTests");
    let decl = &summary.declarations[0];
    assert!(decl.names.is_empty(), "names should stay empty: {:?}", decl.names);
    assert_eq!(decl.binder_kind, BinderKind::Destructuring);
}

// -------------------------------------------------------------------------------------------
// get: `()` and `_` are addressable like any other name.
// -------------------------------------------------------------------------------------------

#[test]
fn get_unit_and_wildcard() {
    let (_dir, path) = scratch_res(
        "let () = {\n  Console.log(\"test 1\")\n}\n\nlet _ = Console.log(\"side effect\")\n",
    );
    let path_str = path.to_str().unwrap().to_string();

    let unit = extract_group(&path, &["()".to_string()]).expect("get () should succeed");
    assert_eq!(unit.len(), 1);
    assert!(unit[0].source.contains("test 1"));

    let wildcard = extract_group(&path, &["_".to_string()]).expect("get _ should succeed");
    assert_eq!(wildcard.len(), 1);
    assert!(wildcard[0].source.contains("side effect"));
    let _ = path_str; // keep the path alive/used for clarity
}

/// Nested: `Inner.()` inside `module Inner = { … }`.
#[test]
fn get_nested_inner_unit() {
    let (_dir, path) = scratch_res(
        "module Inner = {\n  let () = Console.log(\"inner\")\n}\n",
    );
    let result = extract_group(&path, &["Inner.()".to_string()]).expect("get Inner.() failed");
    assert_eq!(result.len(), 1);
    assert!(result[0].source.contains("inner"));
}

/// Two `let () = …` bindings in one module: every command refuses `()` as ambiguous. Existing
/// ambiguity logic (`Outline::resolve` / ambiguity in `extract.rs`) — not changed by this fix, but
/// must still hold now that `()` is a real, matchable name.
#[test]
fn two_unit_bindings_are_ambiguous() {
    let (_dir, path) = scratch_res("let () = Console.log(\"first\")\nlet () = Console.log(\"second\")\n");
    let err = extract_group(&path, &["()".to_string()])
        .expect_err("get () should refuse as ambiguous");
    assert!(
        err.to_string().contains("ambiguous"),
        "expected an ambiguity error, got: {err}"
    );

    let err = patch(&path, "()", "first", "First").expect_err("patch should refuse");
    assert!(err.to_string().contains("ambiguous"), "patch: {err}");

    let err = rm_decl(&path, &["()".to_string()]).expect_err("rm decl should refuse");
    assert!(err.to_string().contains("ambiguous"), "rm decl: {err}");

    let err = set_decl(&path, Some("()"), "let () = Console.log(\"third\")")
        .expect_err("set decl should refuse");
    assert!(err.to_string().contains("ambiguous"), "set decl: {err}");
}

// -------------------------------------------------------------------------------------------
// patch
// -------------------------------------------------------------------------------------------

#[test]
fn patch_unit_binding() {
    let (_dir, path) = scratch_res("let () = {\n  Console.log(\"test 1\")\n}\n");
    patch(&path, "()", "test 1", "test 2").expect("patch () should succeed");
    let updated = std::fs::read_to_string(&path).unwrap();
    assert!(updated.contains("test 2"));
    assert!(!updated.contains("test 1"));
}

#[test]
fn patch_wildcard_binding() {
    let (_dir, path) = scratch_res("let _ = Console.log(\"side effect\")\n");
    patch(&path, "_", "side effect", "logged").expect("patch _ should succeed");
    let updated = std::fs::read_to_string(&path).unwrap();
    assert!(updated.contains("logged"));
}

// -------------------------------------------------------------------------------------------
// rm decl — removing `()` leaves the file clean (and leaves an unrelated `_` binding intact).
// -------------------------------------------------------------------------------------------

#[test]
fn rm_decl_unit_binding_leaves_file_clean() {
    let (_dir, path) = scratch_res(
        "let () = {\n  Console.log(\"test 1\")\n}\n\nlet _ = Console.log(\"side effect\")\n",
    );
    rm_decl(&path, &["()".to_string()]).expect("rm decl () should succeed");
    let updated = std::fs::read_to_string(&path).unwrap();
    assert_eq!(updated, "let _ = Console.log(\"side effect\")\n");

    // The file must still parse clean afterwards.
    let tree = parse(&updated).expect("parses");
    assert!(!tree.root_node().has_error());
}

#[test]
fn rm_decl_wildcard_binding() {
    let (_dir, path) = scratch_res("let () = Console.log(\"keep\")\n\nlet _ = Console.log(\"drop\")\n");
    rm_decl(&path, &["_".to_string()]).expect("rm decl _ should succeed");
    let updated = std::fs::read_to_string(&path).unwrap();
    assert_eq!(updated, "let () = Console.log(\"keep\")\n");
}

// -------------------------------------------------------------------------------------------
// set decl --name '()' replaces the existing binding.
// -------------------------------------------------------------------------------------------

#[test]
fn set_decl_replaces_unit_binding_by_name() {
    let (_dir, path) = scratch_res(
        "let () = {\n  Console.log(\"test 1\")\n}\n\nlet _ = Console.log(\"side effect\")\n",
    );
    set_decl(&path, Some("()"), "let () = Console.log(\"replaced\")").expect("set decl should succeed");
    let updated = std::fs::read_to_string(&path).unwrap();
    assert!(updated.contains("replaced"));
    assert!(!updated.contains("test 1"));
    assert!(updated.contains("side effect"), "unrelated binding must survive");
}

#[test]
fn set_decl_replaces_wildcard_binding_by_name() {
    let (_dir, path) = scratch_res("let _ = Console.log(\"before\")\n");
    set_decl(&path, Some("_"), "let _ = Console.log(\"after\")").expect("set decl should succeed");
    let updated = std::fs::read_to_string(&path).unwrap();
    assert!(updated.contains("after"));
    assert!(!updated.contains("before"));
}

/// TASK 1c: `--name '()'` with no position, and no existing `()` binding, still appends — the
/// same as the implicit-name append case below, exercised through the explicit-name path too.
#[test]
fn explicit_name_unit_appends_when_none_exists() {
    let (_dir, path) = scratch_res("let marker = 1\n");
    set_decl(&path, Some("()"), "let () = Console.log(\"first\")")
        .expect("explicit --name '()' should append when none exists");
    let updated = std::fs::read_to_string(&path).unwrap();
    assert!(updated.contains("marker"));
    assert!(updated.contains("first"));
}

// -------------------------------------------------------------------------------------------
// TASK 1a — `set decl --before`/`--after` on an anonymous leaf always adds a new binding, even
// when one (or more) already exists. `()` and `_` bind no name, so a second one is not a
// collision. See `edit.rs::set_decl_source_at`.
// -------------------------------------------------------------------------------------------

#[test]
fn before_adds_a_second_unit_binding_when_one_already_exists() {
    let (_dir, path) = scratch_res("let () = Console.log(\"first\")\nlet marker = 1\n");
    set_decl_at(&path, Some("()"), "let () = Console.log(\"second\")", Some("marker"), None)
        .expect("--before on `()` should add, not refuse, when one already exists");
    let updated = std::fs::read_to_string(&path).unwrap();
    assert!(updated.contains("first"));
    assert!(updated.contains("second"));
    assert_eq!(updated.matches("let () =").count(), 2);
    let tree = parse(&updated).expect("parses");
    assert!(!tree.root_node().has_error());
}

#[test]
fn after_adds_a_second_wildcard_binding_when_one_already_exists() {
    let (_dir, path) = scratch_res("let marker = 1\nlet _ = Console.log(\"first\")\n");
    set_decl_at(&path, Some("_"), "let _ = Console.log(\"second\")", None, Some("marker"))
        .expect("--after on `_` should add, not refuse, when one already exists");
    let updated = std::fs::read_to_string(&path).unwrap();
    assert!(updated.contains("first"));
    assert!(updated.contains("second"));
    assert_eq!(updated.matches("let _ =").count(), 2);
    let tree = parse(&updated).expect("parses");
    assert!(!tree.root_node().has_error());
}

/// The always-add rule holds even with no `--name` at all — `--before`/`--after` are checked
/// before the implicit-name refusal (TASK 1b) ever applies.
#[test]
fn before_with_no_name_adds_a_second_unit_binding_when_one_already_exists() {
    let (_dir, path) = scratch_res("let marker = 1\nlet () = Console.log(\"first\")\n");
    set_decl_at(&path, None, "let () = Console.log(\"second\")", Some("marker"), None)
        .expect("--before with implicit name should still add, not refuse");
    let updated = std::fs::read_to_string(&path).unwrap();
    assert_eq!(updated.matches("let () =").count(), 2);
    let tree = parse(&updated).expect("parses");
    assert!(!tree.root_node().has_error());
}

// -------------------------------------------------------------------------------------------
// TASK 1b — `set decl` with no position and no `--name`, targeting an anonymous leaf that
// already exists: refuse, leave the file byte-identical, and name the fix.
// -------------------------------------------------------------------------------------------

#[test]
fn implicit_set_decl_refuses_when_unit_binding_already_exists() {
    let (_dir, path) = scratch_res("let () = Console.log(\"first\")\n");
    let before = std::fs::read_to_string(&path).unwrap();
    let err = set_decl(&path, None, "let () = Console.log(\"second\")")
        .expect_err("implicit set decl must refuse when `()` already exists");
    let msg = err.to_string();
    assert!(msg.contains(path.to_str().unwrap()), "message should name the file: {msg}");
    assert!(msg.contains("line 1"), "message should name the existing binding's line: {msg}");
    assert!(msg.contains("--name '()'"), "message should hint --name '()': {msg}");
    assert!(
        msg.contains("--before") && msg.contains("--after"),
        "message should hint --before/--after: {msg}"
    );
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, before, "a refused set decl must leave the file byte-identical");
}

#[test]
fn implicit_set_decl_refuses_when_wildcard_binding_already_exists() {
    let (_dir, path) = scratch_res("let _ = Console.log(\"first\")\n");
    let before = std::fs::read_to_string(&path).unwrap();
    let err = set_decl(&path, None, "let _ = Console.log(\"second\")")
        .expect_err("implicit set decl must refuse when `_` already exists");
    let msg = err.to_string();
    assert!(msg.contains(path.to_str().unwrap()), "message should name the file: {msg}");
    assert!(msg.contains("line 1"), "message should name the existing binding's line: {msg}");
    assert!(msg.contains("--name '_'"), "message should hint --name '_': {msg}");
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, before, "a refused set decl must leave the file byte-identical");
}

/// Two existing `()` bindings: the refusal names every one of them, not just the first.
#[test]
fn implicit_set_decl_refusal_names_every_existing_line() {
    let (_dir, path) =
        scratch_res("let () = Console.log(\"first\")\n\nlet () = Console.log(\"second\")\n");
    let before = std::fs::read_to_string(&path).unwrap();
    let err = set_decl(&path, None, "let () = Console.log(\"third\")")
        .expect_err("implicit set decl must refuse when `()` already exists more than once");
    let msg = err.to_string();
    assert!(msg.contains("lines 1, 3"), "message should name both existing lines: {msg}");
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, before, "a refused set decl must leave the file byte-identical");
}

/// TASK 1b, no-existing-binding case: with no position and no `--name`, and nothing to collide
/// with, an implicit `set decl` still appends — exactly as it does for a named leaf.
#[test]
fn implicit_set_decl_appends_when_no_unit_binding_exists() {
    let (_dir, path) = scratch_res("let marker = 1\n");
    set_decl(&path, None, "let () = Console.log(\"first\")")
        .expect("implicit set decl should append when none exists");
    let updated = std::fs::read_to_string(&path).unwrap();
    assert!(updated.contains("marker"));
    assert!(updated.contains("first"));
}

// -------------------------------------------------------------------------------------------
// refs — audit case: a target that resolves must not turn an unrelated switch-arm `_` (or any
// other bare `_`) into a false-positive reference. `_` is deliberately excluded from
// `refs::collect_occurrences`'s `Occ::Bare` construction for every caller, unconditionally — this
// locks that in now that `_` is itself addressable, so asking about it can no longer regress into
// reporting nonsense.
// -------------------------------------------------------------------------------------------

#[test]
fn refs_on_wildcard_target_finds_no_false_positive_from_unrelated_switch_arm() {
    let (_dir, src) = scratch_project(&[(
        "WithSwitch.res",
        "let _ = Console.log(\"side effect\")\n\n\
         let classify = (x: int) =>\n  switch x {\n  | 0 => \"zero\"\n  | _ => \"other\"\n  }\n",
    )]);
    let file = src.join("WithSwitch.res");
    let refs = find(&file, &["_".to_string()]).expect("refs on `_` should resolve, not error");
    assert!(
        refs.is_empty(),
        "an unrelated switch-arm `_` must never be reported as a reference: {refs:?}"
    );
}

#[test]
fn refs_on_unit_target_does_not_error() {
    let (_dir, src) = scratch_project(&[(
        "AllTests.res",
        "let () = Console.log(\"test 1\")\n",
    )]);
    let file = src.join("AllTests.res");
    let refs = find(&file, &["()".to_string()]).expect("refs on `()` should resolve, not error");
    assert!(refs.is_empty(), "`()` cannot be referenced from elsewhere: {refs:?}");
}
