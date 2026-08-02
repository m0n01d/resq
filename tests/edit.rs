//! Wave 3 (agent A9) — tests for `src/edit.rs`: `set decl`, `patch`, `rm decl`.
//!
//! **Nothing here writes to `tests/fixtures/`.** Every mutating test copies the shared fixture
//! project into a `tempfile::TempDir` first — a test that mutated the fixtures would corrupt every
//! other agent's suite. `fixture_project()` and `scratch_file()` are the only two ways in.
//!
//! The required cases, in order of appearance:
//!
//! 1. `rm decl Main.res entry` takes the declaration, its `@genType` **and** its doc comment.
//! 2. `rm decl` does not orphan a decorator onto the following declaration.
//! 3. `rm decl View.res polyColor` refuses (the `.resi` sync guard) and changes neither file.
//! 4. `rm decl` on a copy of `View.resi` succeeds — the reason resq needs no `expose`/`unexpose`.
//! 5. `set decl` replaces an existing declaration and appends a new one; both re-parse clean.
//! 6. `set decl` with invalid `--content` aborts, leaving the file byte-identical.
//! 7. `patch` errors on two matches and on zero matches, and succeeds on exactly one.
//! 8. Every write command refuses `tests/fixtures/broken.res`, writing zero bytes.

use resq::edit::{check_resi_sync, choose_content, patch, rm_decl, set_decl};
use resq::{ModulePath, parser};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const FIXTURES: &str = "tests/fixtures";

// ---------------------------------------------------------------------------------------------
// Scratch helpers — the fixtures are read-only, always.
// ---------------------------------------------------------------------------------------------

/// A throwaway copy of `tests/fixtures/proj`. Returns the `TempDir` (which must stay alive for the
/// duration of the test) and the path of its `src` directory.
fn fixture_project() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let dest = dir.path().join("proj");
    copy_dir(Path::new(FIXTURES).join("proj").as_path(), &dest);
    let src = dest.join("src");
    (dir, src)
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create scratch dir");
    for entry in std::fs::read_dir(from).expect("read fixture dir") {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy fixture file");
        }
    }
}

/// A scratch file with the given contents, in its own temp dir (so `.res`/`.resi` siblings can be
/// created deliberately and never by accident).
fn scratch_file(name: &str, contents: &str) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(name);
    std::fs::write(&path, contents).expect("write scratch file");
    (dir, path)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).expect("read scratch file")
}

/// Assert the file parses with no ERROR/MISSING nodes — the property every write command promises.
fn assert_reparses_clean(path: &Path) {
    let src = read(path);
    let tree = parser::parse(&src).expect("parse");
    assert!(
        !tree.root_node().has_error(),
        "{} does not re-parse clean:\n{src}",
        path.display()
    );
}

fn paths(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| n.to_string()).collect()
}

// ---------------------------------------------------------------------------------------------
// 1. `rm decl` removes the declaration, its decorator, and its doc comment.
// ---------------------------------------------------------------------------------------------

#[test]
fn rm_decl_removes_declaration_decorator_and_doc_comment() {
    let (_dir, src) = fixture_project();
    let main = src.join("Main.res");
    let before = read(&main);
    assert!(before.contains("/** Top-level entry point. */"));
    assert!(before.contains("@genType"));

    rm_decl(&main, &paths(&["entry"])).expect("rm decl entry");

    let after = read(&main);
    assert!(
        !after.contains("let entry"),
        "declaration survived:\n{after}"
    );
    assert!(
        !after.contains("@genType"),
        "decorator survived — it would orphan onto the next declaration:\n{after}"
    );
    assert!(
        !after.contains("/** Top-level entry point. */"),
        "doc comment survived:\n{after}"
    );
    // Untouched neighbours are still there, and the file still parses.
    assert!(after.contains("let (first, second) = (1, 2)"));
    assert!(after.contains("open Belt"));
    assert_reparses_clean(&main);
}

#[test]
fn rm_decl_removes_a_nested_declaration_with_its_doc_comment() {
    let (_dir, src) = fixture_project();
    let main = src.join("Main.res");
    rm_decl(&main, &paths(&["Inner.helper"])).expect("rm decl Inner.helper");

    let after = read(&main);
    assert!(!after.contains("let helper"));
    assert!(!after.contains("/** Nested helper. */"));
    assert!(after.contains("let deepValue = 42"), "sibling survived");
    assert_reparses_clean(&main);
}

// ---------------------------------------------------------------------------------------------
// 2. `rm decl` must not orphan a decorator onto the FOLLOWING declaration (SPEC §1 finding 1).
// ---------------------------------------------------------------------------------------------

#[test]
fn rm_decl_does_not_orphan_a_decorator_onto_the_next_declaration() {
    let (_dir, file) = scratch_file(
        "Orphan.res",
        "@genType\nlet doomed = 1\n\n@react.component\nlet survivor = 2\n",
    );
    rm_decl(&file, &paths(&["doomed"])).expect("rm decl doomed");

    let after = read(&file);
    assert_eq!(
        after.matches('@').count(),
        1,
        "exactly one decorator should remain:\n{after}"
    );
    assert!(!after.contains("@genType"), "orphaned decorator:\n{after}");
    assert!(after.contains("@react.component"));
    assert!(after.contains("let survivor = 2"));
    // The surviving decorator must still sit immediately above its own declaration.
    assert!(after.contains("@react.component\nlet survivor = 2"));
    assert_reparses_clean(&file);
}

/// `parser::decl_span_with_attachments` deliberately keeps a decorator that is separated from its
/// declaration by a blank line (`@genType\n\nlet x = 1` still decorates `x`), while a doc comment
/// across a blank line is free-standing prose. Removing `x` must therefore take the decorator and
/// leave the prose.
#[test]
fn rm_decl_takes_a_decorator_across_a_blank_line_but_not_free_standing_prose() {
    let (_dir, file) = scratch_file(
        "Gap.res",
        "/** Free-standing prose. */\n\n@genType\n\nlet target = 1\n\nlet other = 2\n",
    );
    rm_decl(&file, &paths(&["target"])).expect("rm decl target");

    let after = read(&file);
    assert!(
        !after.contains("@genType"),
        "decorator across a blank line still binds and must go:\n{after}"
    );
    assert!(
        after.contains("/** Free-standing prose. */"),
        "prose separated by a blank line is not a doc comment and must stay:\n{after}"
    );
    assert!(after.contains("let other = 2"));
    assert_reparses_clean(&file);
}

/// Several paths in one invocation, including two names of a single destructuring binding.
#[test]
fn rm_decl_removes_several_paths_including_both_names_of_one_binding() {
    let (_dir, src) = fixture_project();
    let main = src.join("Main.res");
    rm_decl(&main, &paths(&["first", "second", "entry"])).expect("rm decl first second entry");

    let after = read(&main);
    assert!(!after.contains("let (first, second)"));
    assert!(!after.contains("let entry"));
    assert!(after.contains("external evalRaw"));
    assert_reparses_clean(&main);
}

/// SPEC §3.7 decision: removing one name of a multi-name binding is REFUSED, because there is no
/// smaller thing to delete and dropping the binding would silently unbind the other name.
#[test]
fn rm_decl_refuses_to_partially_remove_a_multi_name_binding() {
    let (_dir, src) = fixture_project();
    let main = src.join("Main.res");
    let before = read(&main);

    let err = rm_decl(&main, &paths(&["first"])).expect_err("partial removal must be refused");
    let msg = err.to_string();
    assert!(
        msg.contains("second"),
        "error should name the orphan: {msg}"
    );
    assert_eq!(read(&main), before, "file must be untouched");
}

#[test]
fn rm_decl_on_an_unknown_path_errors_and_writes_nothing() {
    let (_dir, src) = fixture_project();
    let main = src.join("Main.res");
    let before = read(&main);
    rm_decl(&main, &paths(&["noSuchThing"])).expect_err("unknown path must error");
    assert_eq!(read(&main), before);
}

// ---------------------------------------------------------------------------------------------
// 3. The `.resi` sync guard (SPEC §3.3).
// ---------------------------------------------------------------------------------------------

#[test]
fn rm_decl_refuses_when_the_sibling_resi_still_declares_the_name() {
    let (_dir, src) = fixture_project();
    let view = src.join("View.res");
    let view_i = src.join("View.resi");
    let before_res = read(&view);
    let before_resi = read(&view_i);

    let err = rm_decl(&view, &paths(&["polyColor"])).expect_err("the .resi guard must refuse");
    let msg = err.to_string();
    assert!(
        msg.contains("polyColor"),
        "error should name the orphan: {msg}"
    );
    assert!(
        msg.contains("View.resi"),
        "error should name the .resi: {msg}"
    );

    assert_eq!(read(&view), before_res, "View.res must be unchanged");
    assert_eq!(read(&view_i), before_resi, "View.resi must be unchanged");
}

#[test]
fn the_guard_also_refuses_for_make_and_reports_every_orphan_at_once() {
    let (_dir, src) = fixture_project();
    let view = src.join("View.res");
    let err = rm_decl(&view, &paths(&["make", "polyColor"])).expect_err("guard must refuse");
    let msg = err.to_string();
    assert!(msg.contains("make"), "{msg}");
    assert!(msg.contains("polyColor"), "{msg}");
}

/// The guard is about the *pair*: with no sibling `.resi`, removal proceeds normally.
#[test]
fn rm_decl_proceeds_when_there_is_no_sibling_resi() {
    let (_dir, src) = fixture_project();
    let view = src.join("View.res");
    std::fs::remove_file(src.join("View.resi")).expect("drop the interface file");

    rm_decl(&view, &paths(&["polyColor"])).expect("no sibling means no guard");
    let after = read(&view);
    assert!(!after.contains("polyColor"));
    assert!(after.contains("@react.component"));
    assert_reparses_clean(&view);
}

/// ...and it only fires for names the `.resi` actually lists.
#[test]
fn the_guard_allows_removing_a_name_the_resi_does_not_declare() {
    let (_dir, src) = fixture_project();
    let view = src.join("View.res");
    let mut contents = read(&view);
    contents.push_str("\nlet privateHelper = 3\n");
    std::fs::write(&view, &contents).expect("extend the scratch copy");

    rm_decl(&view, &paths(&["privateHelper"])).expect("not in the .resi, so not guarded");
    let after = read(&view);
    assert!(!after.contains("privateHelper"));
    assert!(after.contains("polyColor"), "guarded neighbour untouched");
    assert_reparses_clean(&view);
}

#[test]
fn check_resi_sync_is_a_no_op_for_a_resi_input() {
    let (_dir, src) = fixture_project();
    check_resi_sync(&src.join("View.resi"), &[ModulePath::parse("polyColor")])
        .expect("editing the interface itself is never guarded");
}

// ---------------------------------------------------------------------------------------------
// 4. `.resi` files are edited with the same commands — the reason there is no `expose`/`unexpose`.
// ---------------------------------------------------------------------------------------------

#[test]
fn rm_decl_works_on_a_resi_file() {
    let (_dir, src) = fixture_project();
    let view_i = src.join("View.resi");
    let view = src.join("View.res");
    let before_res = read(&view);

    rm_decl(&view_i, &paths(&["polyColor"])).expect("rm decl on a .resi must succeed");

    let after = read(&view_i);
    assert!(!after.contains("polyColor"), "signature survived:\n{after}");
    assert!(
        after.contains("let make: (~name: string, ~count: int) => React.element"),
        "the other signature must survive:\n{after}"
    );
    assert!(after.contains("@react.component"));
    assert_reparses_clean(&view_i);
    assert_eq!(read(&view), before_res, "the .res must not be touched");
}

/// The full unexpose-then-remove workflow the SPEC prescribes in place of an `unexpose` command:
/// edit the `.resi` first, then the `.res` removal is allowed.
#[test]
fn removing_from_the_resi_first_unblocks_the_res_removal() {
    let (_dir, src) = fixture_project();
    let view = src.join("View.res");
    let view_i = src.join("View.resi");

    rm_decl(&view, &paths(&["polyColor"])).expect_err("guarded while the signature exists");
    rm_decl(&view_i, &paths(&["polyColor"])).expect("remove the signature");
    rm_decl(&view, &paths(&["polyColor"])).expect("now the implementation may go");

    assert!(!read(&view).contains("polyColor"));
    assert!(!read(&view_i).contains("polyColor"));
    assert_reparses_clean(&view);
    assert_reparses_clean(&view_i);
}

#[test]
fn set_decl_and_patch_work_on_a_resi_file_too() {
    let (_dir, src) = fixture_project();
    let view_i = src.join("View.resi");

    set_decl(&view_i, Some("label"), "let label: string").expect("append a signature");
    assert!(read(&view_i).contains("let label: string"));

    patch(&view_i, "label", "string", "int").expect("patch a signature");
    assert!(read(&view_i).contains("let label: int"));
    assert_reparses_clean(&view_i);
}

// ---------------------------------------------------------------------------------------------
// 5. `set decl` — replace an existing declaration, append a new one.
// ---------------------------------------------------------------------------------------------

#[test]
fn set_decl_replaces_an_existing_declaration() {
    let (_dir, src) = fixture_project();
    let main = src.join("Main.res");

    set_decl(&main, Some("entry"), "@genType\nlet entry = () => 99").expect("replace entry");

    let after = read(&main);
    assert!(after.contains("let entry = () => 99"), "{after}");
    assert!(!after.contains("let entry = () => 1"), "{after}");
    assert_eq!(
        after.matches("let entry").count(),
        1,
        "replacement must not duplicate the declaration:\n{after}"
    );
    // The old doc comment is part of the replaced span, so it goes with the old body.
    assert!(!after.contains("/** Top-level entry point. */"));
    assert!(after.contains("@genType"));
    assert_reparses_clean(&main);
}

#[test]
fn set_decl_appends_a_new_top_level_declaration() {
    let (_dir, src) = fixture_project();
    let main = src.join("Main.res");

    set_decl(&main, None, "/** Brand new. */\nlet freshValue = 7").expect("append freshValue");

    let after = read(&main);
    assert!(after.contains("let freshValue = 7"), "{after}");
    assert!(after.contains("/** Brand new. */"));
    assert!(after.ends_with('\n'), "file should end with a newline");
    assert!(
        after.contains("let unicodeString"),
        "existing declarations survive"
    );
    assert_reparses_clean(&main);
}

#[test]
fn set_decl_replaces_and_appends_inside_a_nested_module() {
    let (_dir, src) = fixture_project();
    let main = src.join("Main.res");

    set_decl(&main, Some("Inner.Deep.deepValue"), "let deepValue = 43").expect("replace nested");
    set_decl(&main, Some("Inner.Deep.extra"), "let extra = 44").expect("append nested");

    let after = read(&main);
    assert!(after.contains("let deepValue = 43"), "{after}");
    assert!(!after.contains("let deepValue = 42"));
    assert!(after.contains("let extra = 44"), "{after}");
    // Appended inside `Deep`, not at the file root: it must precede the module's closing braces.
    let extra_at = after.find("let extra = 44").expect("appended");
    let unicode_at = after.find("let unicodeString").expect("still there");
    assert!(
        extra_at < unicode_at,
        "appended in the wrong scope:\n{after}"
    );
    assert_reparses_clean(&main);

    // And it is addressable at the path it was created under.
    let found = resq::extract::extract_group(&main, &["Inner.Deep.extra".to_string()])
        .expect("the new declaration resolves at its dot-path");
    assert_eq!(found[0].source, "let extra = 44");
}

#[test]
fn set_decl_refuses_a_path_whose_parent_module_does_not_exist() {
    let (_dir, src) = fixture_project();
    let main = src.join("Main.res");
    let before = read(&main);

    let err = set_decl(&main, Some("Nope.thing"), "let thing = 1")
        .expect_err("a missing parent module must be an error, not a silent top-level append");
    assert!(err.to_string().contains("Nope"), "{err}");
    assert_eq!(read(&main), before);
}

#[test]
fn set_decl_refuses_when_the_content_name_disagrees_with_the_given_name() {
    let (_dir, src) = fixture_project();
    let main = src.join("Main.res");
    let before = read(&main);

    let err = set_decl(&main, Some("entry"), "let somethingElse = 1")
        .expect_err("a name mismatch is an error, not a silent rename");
    let msg = err.to_string();
    assert!(msg.contains("somethingElse"), "{msg}");
    assert!(msg.contains("entry"), "{msg}");
    assert_eq!(read(&main), before, "file must be byte-identical");
}

/// Same hazard as `rm decl`: replacing `let (first, second) = …` with a binding for one of the two
/// names would silently unbind the other.
#[test]
fn set_decl_refuses_to_replace_a_multi_name_binding_with_a_single_name() {
    let (_dir, src) = fixture_project();
    let main = src.join("Main.res");
    let before = read(&main);

    let err = set_decl(&main, Some("first"), "let first = 9")
        .expect_err("replacing a multi-name binding partially must be refused");
    assert!(err.to_string().contains("second"), "{err}");
    assert_eq!(read(&main), before);

    // Providing content that binds both names is allowed.
    set_decl(&main, Some("first"), "let (first, second) = (9, 10)").expect("full replacement");
    assert!(read(&main).contains("let (first, second) = (9, 10)"));
    assert_reparses_clean(&main);
}

// ---------------------------------------------------------------------------------------------
// 6. Invalid `--content` aborts and leaves the file byte-identical.
// ---------------------------------------------------------------------------------------------

#[test]
fn set_decl_with_invalid_content_aborts_and_leaves_the_file_byte_identical() {
    let (_dir, src) = fixture_project();
    let main = src.join("Main.res");
    let before = read(&main);

    set_decl(&main, Some("busted"), "let busted = (x => {")
        .expect_err("syntactically invalid content must be refused");

    assert_eq!(read(&main), before, "file must be byte-for-byte unchanged");
    assert_reparses_clean(&main);
}

#[test]
fn set_decl_rejects_empty_and_multi_declaration_content() {
    let (_dir, src) = fixture_project();
    let main = src.join("Main.res");
    let before = read(&main);

    set_decl(&main, Some("x"), "   \n  ").expect_err("empty content");
    set_decl(&main, Some("x"), "// just a comment").expect_err("no declaration in content");
    set_decl(&main, Some("x"), "let x = 1\nlet y = 2")
        .expect_err("two declarations cannot upsert at one path");

    assert_eq!(read(&main), before);
}

/// `--content` and stdin are exactly-one-of.
#[test]
fn choose_content_requires_exactly_one_source() {
    assert_eq!(
        choose_content(Some("let a = 1".into()), None).expect("flag only"),
        "let a = 1"
    );
    assert_eq!(
        choose_content(None, Some("let a = 1".into())).expect("stdin only"),
        "let a = 1"
    );
    let both = choose_content(Some("let a = 1".into()), Some("let b = 2".into()))
        .expect_err("both is an error");
    assert!(both.to_string().contains("exactly one"), "{both}");
    let neither = choose_content(None, None).expect_err("neither is an error");
    assert!(neither.to_string().contains("--content"), "{neither}");
}

// ---------------------------------------------------------------------------------------------
// 7. `patch` — exactly once, or nothing.
// ---------------------------------------------------------------------------------------------

#[test]
fn patch_with_exactly_one_match_succeeds() {
    let (_dir, src) = fixture_project();
    let main = src.join("Main.res");

    patch(&main, "update", "Types.Reset => 0", "Types.Reset => 100").expect("single match");

    let after = read(&main);
    assert!(after.contains("| Types.Reset => 100"), "{after}");
    assert!(after.contains("| Types.Increment => count + 1"), "{after}");
    assert_reparses_clean(&main);
}

#[test]
fn patch_with_two_matches_errors_and_writes_nothing() {
    let (_dir, src) = fixture_project();
    let main = src.join("Main.res");
    let before = read(&main);

    // `count` appears four times inside `update`.
    let err = patch(&main, "update", "count", "n").expect_err("multiple matches must error");
    let msg = err.to_string();
    assert!(msg.contains("exactly once"), "{msg}");
    assert_eq!(read(&main), before, "file must be untouched");
}

#[test]
fn patch_with_zero_matches_errors_and_writes_nothing() {
    let (_dir, src) = fixture_project();
    let main = src.join("Main.res");
    let before = read(&main);

    let err = patch(&main, "update", "no such text", "x").expect_err("no match must error");
    assert!(err.to_string().contains("not found"), "{err}");
    assert_eq!(read(&main), before);
}

/// The match is scoped to the named declaration: text that occurs elsewhere in the file does not
/// count, and is not rewritten.
#[test]
fn patch_is_scoped_to_the_named_declaration() {
    let (_dir, file) = scratch_file(
        "Scope.res",
        "let a = \"target\"\n\nlet b = \"target\"\n\nlet c = \"target\"\n",
    );
    patch(&file, "b", "target", "changed").expect("one match inside `b`");

    let after = read(&file);
    assert_eq!(
        after,
        "let a = \"target\"\n\nlet b = \"changed\"\n\nlet c = \"target\"\n"
    );
    assert_reparses_clean(&file);
}

/// The scope includes the declaration's attachments, so a decorator or doc comment can be patched.
#[test]
fn patch_can_reach_the_decorator_and_doc_comment() {
    let (_dir, src) = fixture_project();
    let main = src.join("Main.res");

    patch(&main, "entry", "Top-level entry point.", "Entry point.").expect("patch the doc comment");
    assert!(read(&main).contains("/** Entry point. */"));

    patch(&main, "entry", "@genType", "@genType.import(\"./x\")").expect("patch the decorator");
    let after = read(&main);
    assert!(after.contains("@genType.import(\"./x\")"), "{after}");
    assert_reparses_clean(&main);
}

#[test]
fn patch_rejects_an_empty_old_string_and_an_unknown_path() {
    let (_dir, src) = fixture_project();
    let main = src.join("Main.res");
    let before = read(&main);

    patch(&main, "entry", "", "x").expect_err("empty --old must be refused");
    patch(&main, "noSuchDeclaration", "a", "b").expect_err("unknown path must error");
    assert_eq!(read(&main), before);
}

/// Write-safety step 2: a patch that produces unparseable output is rejected *after* splicing, and
/// the file is left alone. This is the check that catches our own bugs, not just the user's.
#[test]
fn patch_that_would_break_the_file_is_rejected_and_writes_nothing() {
    let (_dir, file) = scratch_file("Break.res", "let value = 1 + 2\n");
    let before = read(&file);

    let err = patch(&file, "value", "1 + 2", "1 +").expect_err("broken output must be refused");
    let msg = err.to_string();
    assert!(msg.contains("does not parse"), "{msg}");
    assert!(msg.contains("unchanged"), "{msg}");
    assert_eq!(read(&file), before, "file must be byte-for-byte unchanged");
}

// ---------------------------------------------------------------------------------------------
// 8. Every write command refuses a file that does not parse, writing zero bytes.
// ---------------------------------------------------------------------------------------------

#[test]
fn every_write_command_refuses_a_file_with_pre_existing_parse_errors() {
    let broken_source = std::fs::read_to_string(Path::new(FIXTURES).join("broken.res"))
        .expect("read the broken fixture");
    let (_dir, file) = scratch_file("broken.res", &broken_source);

    let checks: Vec<(&str, anyhow::Error)> = vec![
        (
            "set decl",
            set_decl(&file, Some("added"), "let added = 1").expect_err("set decl must refuse"),
        ),
        (
            "patch",
            patch(&file, "broken", "x", "y").expect_err("patch must refuse"),
        ),
        (
            "rm decl",
            rm_decl(&file, &paths(&["broken"])).expect_err("rm decl must refuse"),
        ),
    ];

    for (op, err) in checks {
        let msg = err.to_string();
        assert!(
            msg.contains("pre-existing parse errors"),
            "{op} should refuse on the pre-parse check: {msg}"
        );
        assert!(
            msg.contains("broken.res"),
            "{op} should name the file: {msg}"
        );
    }

    assert_eq!(
        read(&file),
        broken_source,
        "the broken file must be byte-for-byte unchanged"
    );
}

/// ...and the same holds when the *sibling* `.resi` is the unparseable one: an invariant we cannot
/// verify is a refusal, not a pass.
#[test]
fn rm_decl_refuses_when_the_sibling_resi_does_not_parse() {
    let (_dir, src) = fixture_project();
    let view = src.join("View.res");
    let before = read(&view);
    std::fs::write(src.join("View.resi"), "let make: (\n").expect("break the interface file");

    let err = rm_decl(&view, &paths(&["make"])).expect_err("an unverifiable guard must refuse");
    let msg = err.to_string();
    assert!(msg.contains("does not parse"), "{msg}");
    assert!(msg.contains("sync guard"), "{msg}");
    assert_eq!(read(&view), before);
}

// ---------------------------------------------------------------------------------------------
// Whitespace hygiene: `rm decl` must not leave a widening gap or an unbalanced blank line.
// ---------------------------------------------------------------------------------------------

#[test]
fn rm_decl_collapses_the_blank_lines_it_leaves_behind() {
    let (_dir, file) = scratch_file("Gaps.res", "let a = 1\n\nlet b = 2\n\nlet c = 3\n");
    rm_decl(&file, &paths(&["b"])).expect("rm decl b");
    assert_eq!(read(&file), "let a = 1\n\nlet c = 3\n");
}

#[test]
fn rm_decl_of_the_last_declaration_ends_the_file_with_one_newline() {
    let (_dir, file) = scratch_file("Tail.res", "let a = 1\n\nlet b = 2\n");
    rm_decl(&file, &paths(&["b"])).expect("rm decl b");
    assert_eq!(read(&file), "let a = 1\n");
}

#[test]
fn rm_decl_of_the_first_declaration_does_not_leave_a_leading_blank_line() {
    let (_dir, file) = scratch_file("Head.res", "let a = 1\n\nlet b = 2\n");
    rm_decl(&file, &paths(&["a"])).expect("rm decl a");
    assert_eq!(read(&file), "let b = 2\n");
}

#[test]
fn rm_decl_of_a_lone_module_member_leaves_a_tidy_block() {
    let (_dir, file) = scratch_file(
        "Block.res",
        "module Inner = {\n  let only = 1\n}\n\nlet after = 2\n",
    );
    rm_decl(&file, &paths(&["Inner.only"])).expect("rm decl Inner.only");
    assert_eq!(read(&file), "module Inner = {\n}\n\nlet after = 2\n");
    assert_reparses_clean(&file);
}
