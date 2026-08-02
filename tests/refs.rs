//! Wave 3 (agent A7) — `resq refs`: project-wide reference resolution.
//!
//! Exercises `resq::refs::{find, run}` against `tests/fixtures/proj/`, plus a handful of synthetic
//! tempdir projects for scope shapes the shared fixture does not contain (an `open`ed target, a
//! shadowing binder, a `.resi` that references a *third* module). The fixture is conductor-owned
//! and must not be edited, so anything needing new ReScript gets its own throwaway project.
//!
//! `find` is the pure seam — it returns the reference list without printing, so classifications and
//! enclosing dot-paths can be asserted directly.

use resq::cli::Format;
use resq::refs::{RefKind, Reference, find, run};
use std::path::{Path, PathBuf};

const TYPES: &str = "tests/fixtures/proj/src/Types.res";
const MAIN: &str = "tests/fixtures/proj/src/Main.res";
const VIEW: &str = "tests/fixtures/proj/src/View.res";
const UTIL: &str = "tests/fixtures/proj/src/nested/Util.res";

fn refs(file: &str, names: &[&str]) -> Vec<Reference> {
    let owned: Vec<String> = names.iter().map(|s| s.to_string()).collect();
    find(Path::new(file), &owned).expect("refs should succeed")
}

fn in_file<'a>(rs: &'a [Reference], suffix: &str) -> Vec<&'a Reference> {
    rs.iter().filter(|r| r.file.ends_with(suffix)).collect()
}

/// A throwaway ReScript project. The shared fixture is conductor-owned, so scope shapes it does not
/// cover get built here instead.
struct TempProject {
    dir: tempfile::TempDir,
}

impl TempProject {
    fn new(files: &[(&str, &str)]) -> TempProject {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("rescript.json"),
            r#"{ "name": "tmp-proj", "sources": [{ "dir": "src", "subdirs": true }] }"#,
        )
        .unwrap();
        for (name, contents) in files {
            let path = dir.path().join("src").join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        TempProject { dir }
    }

    fn src(&self, name: &str) -> PathBuf {
        self.dir.path().join("src").join(name)
    }

    fn refs(&self, name: &str, names: &[&str]) -> Vec<Reference> {
        let owned: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        find(&self.src(name), &owned).expect("refs should succeed")
    }
}

// -------------------------------------------------------------------------------------------
// 1. `refs <FILE>` finds the qualified uses of the module that file defines.
// -------------------------------------------------------------------------------------------

#[test]
fn module_target_finds_every_qualified_use_in_main() {
    let rs = refs(TYPES, &[]);
    let main = in_file(&rs, "Main.res");

    // Main.res qualifies `Types` four times: the `Types.msg` annotation and three
    // `Types.<Constructor>` switch patterns.
    let lines: Vec<usize> = main.iter().map(|r| r.line).collect();
    assert_eq!(
        lines,
        vec![12, 14, 15, 16],
        "expected the four `Types.*` uses in Main.res, got {main:#?}"
    );
    assert!(
        main.iter().all(|r| r.kind == RefKind::Qualified),
        "every `Types.*` in Main.res is spelled with the module's real name"
    );
    assert!(
        main.iter().all(|r| r.target == "Types"),
        "the target is the module itself"
    );

    // The file that defines the module is itself reported, so the result is never an unexplained
    // empty-looking list.
    let defs: Vec<&Reference> = rs
        .iter()
        .filter(|r| r.kind == RefKind::Definition)
        .collect();
    assert_eq!(defs.len(), 1);
    assert!(defs[0].file.ends_with("Types.res"));
    assert_eq!((defs[0].line, defs[0].column), (1, 1));
}

#[test]
fn module_target_does_not_leak_into_unrelated_files() {
    let rs = refs(TYPES, &[]);
    assert!(
        in_file(&rs, "Modern.res").is_empty(),
        "Modern.res never mentions Types"
    );
    assert!(in_file(&rs, "Util.res").is_empty());
}

// -------------------------------------------------------------------------------------------
// 2. `refs <FILE> <PATH>` finds the type-annotation use in Main.res's `update`.
// -------------------------------------------------------------------------------------------

#[test]
fn declaration_target_finds_the_type_annotation_use() {
    let rs = refs(TYPES, &["msg"]);
    let main = in_file(&rs, "Main.res");

    assert_eq!(main.len(), 1, "exactly one `Types.msg` in Main.res");
    let hit = main[0];
    assert_eq!(hit.line, 12);
    assert_eq!(hit.kind, RefKind::Qualified);
    assert_eq!(hit.text, "Types.msg");
    assert_eq!(hit.target, "Types.msg");
    assert_eq!(
        hit.path.as_ref().map(ToString::to_string).as_deref(),
        Some("update"),
        "the annotation sits in `let update = …`"
    );

    // `Types.Increment` is a *constructor* of `msg`, not the type name — it must not be conflated.
    assert!(
        !main.iter().any(|r| r.text.contains("Increment")),
        "constructor uses are not uses of the type name"
    );
}

#[test]
fn declaration_target_reports_its_own_definition_site() {
    let rs = refs(TYPES, &["msg"]);
    let def = rs
        .iter()
        .find(|r| r.kind == RefKind::Definition)
        .expect("the definition of `msg` in Types.res");
    assert!(def.file.ends_with("Types.res"));
    assert_eq!(def.line, 2, "`type msg =` is on line 2");
    assert_eq!(def.text, "msg");
}

#[test]
fn a_declaration_that_nothing_references_yields_only_its_definition() {
    // `Types.user` is unused across the fixture. The command must still say *something* — a bare
    // empty list is the failure mode this project treats as dangerous.
    let rs = refs(TYPES, &["user"]);
    assert_eq!(rs.len(), 1);
    assert_eq!(rs[0].kind, RefKind::Definition);
}

// -------------------------------------------------------------------------------------------
// 3. A reference inside a nested module reports the correct enclosing dot-path.
// -------------------------------------------------------------------------------------------

#[test]
fn reference_inside_a_nested_module_reports_the_full_enclosing_path() {
    let proj = TempProject::new(&[
        ("Types.res", "let zero = 0\n"),
        (
            "User.res",
            "module Inner = {\n  module Deep = {\n    let nested = Types.zero\n  }\n}\n",
        ),
    ]);
    let rs = proj.refs("Types.res", &["zero"]);
    let user = in_file(&rs, "User.res");

    assert_eq!(user.len(), 1);
    assert_eq!(
        user[0].path.as_ref().map(ToString::to_string).as_deref(),
        Some("Inner.Deep.nested"),
        "the enclosing dot-path must reach through both module levels"
    );
    assert_eq!(user[0].kind, RefKind::Qualified);
}

#[test]
fn nested_declaration_in_the_fixture_reports_its_own_nested_path() {
    // Same property, asserted against the conductor-owned fixture: `Inner.Deep.deepValue`'s
    // definition site must be annotated with the two-level path, not a bare name.
    let rs = refs(MAIN, &["Inner.Deep.deepValue"]);
    assert_eq!(rs.len(), 1);
    assert_eq!(rs[0].kind, RefKind::Definition);
    assert_eq!(
        rs[0].path.as_ref().map(ToString::to_string).as_deref(),
        Some("Inner.Deep.deepValue")
    );
}

// -------------------------------------------------------------------------------------------
// 4. References in a `.resi` are found.
// -------------------------------------------------------------------------------------------

#[test]
fn a_resi_signature_entry_is_reported_as_a_definition() {
    // View.res/View.resi are a pair. A rename of `polyColor` must touch both files, so the
    // signature entry has to surface — missing it is exactly the silent breakage `refs` exists to
    // prevent. This also proves `.resi` files are scanned at all.
    let rs = refs(VIEW, &["polyColor"]);
    let resi = in_file(&rs, "View.resi");
    assert_eq!(resi.len(), 1, "the .resi signature line, got {rs:#?}");
    assert_eq!(resi[0].line, 4);
    assert_eq!(resi[0].kind, RefKind::Definition);
}

#[test]
fn a_reference_that_exists_only_in_a_resi_is_found() {
    // The literal case from the acceptance list: `View.resi` references `React.element`, and that
    // reference exists *nowhere* in the `.res`. The fixture has no `React.res`, so the target
    // module gets its own throwaway project.
    let proj = TempProject::new(&[
        ("React.res", "type element = string\nlet string = x => x\n"),
        (
            "Widget.res",
            "@react.component\nlet make = (~name: string) => name\n",
        ),
        (
            "Widget.resi",
            "@react.component\nlet make: (~name: string) => React.element\n",
        ),
    ]);

    let rs = proj.refs("React.res", &["element"]);
    let resi = in_file(&rs, "Widget.resi");
    assert_eq!(
        resi.len(),
        1,
        "`React.element` appears only in the .resi, got {rs:#?}"
    );
    assert_eq!(resi[0].line, 2);
    assert_eq!(resi[0].kind, RefKind::Qualified);
    assert_eq!(resi[0].text, "React.element");
    assert_eq!(
        resi[0].path.as_ref().map(ToString::to_string).as_deref(),
        Some("make")
    );
}

// -------------------------------------------------------------------------------------------
// 5. Missing project root → clear error, non-zero.
// -------------------------------------------------------------------------------------------

#[test]
fn a_file_with_no_project_root_is_a_clear_error() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Orphan.res");
    std::fs::write(&file, "let a = 1\n").unwrap();

    let err = find(&file, &[]).expect_err("no rescript.json anywhere above a tempdir");
    let message = format!("{err:#}");
    assert!(
        message.contains("project root"),
        "the error must name the missing project root: {message}"
    );
    assert!(
        message.contains("rescript.json"),
        "and say what file it looked for: {message}"
    );

    // `run` is what `main` calls; an `Err` there is what makes the process exit non-zero.
    assert!(run(file, Vec::new(), Format::Compact).is_err());
}

#[test]
fn an_unknown_dot_path_is_a_clear_error() {
    let err = find(Path::new(TYPES), &["noSuchThing".to_string()])
        .expect_err("there is no `noSuchThing` in Types.res");
    let message = format!("{err:#}");
    assert!(message.contains("noSuchThing"), "{message}");
}

// -------------------------------------------------------------------------------------------
// 6. A polymorphic-variant target reports "unsupported", never zero results (SPEC §3.10).
// -------------------------------------------------------------------------------------------

#[test]
fn a_polymorphic_variant_target_is_explicitly_unsupported() {
    let err =
        find(Path::new(VIEW), &["#red".to_string()]).expect_err("polyvars are out of scope for v1");
    let message = format!("{err:#}");
    assert!(
        message.contains("unsupported"),
        "must say `unsupported`, not return an empty list: {message}"
    );
    assert!(message.contains("3.10"), "cite the spec section: {message}");

    // Also true for a polyvar buried inside a dot-path.
    assert!(find(Path::new(VIEW), &["Inner.#red".to_string()]).is_err());
}

#[test]
fn a_polymorphic_variant_type_still_reports_its_type_name_references() {
    // The `#` constructors are unsupported, but the *type name* is an ordinary reference and must
    // not be dropped along with them. (The warning about the constructors goes to stderr.)
    let proj = TempProject::new(&[
        ("Palette.res", "type color = [#red | #green]\n"),
        ("Use.res", "let c: Palette.color = #red\n"),
    ]);
    let rs = proj.refs("Palette.res", &["color"]);
    let use_ = in_file(&rs, "Use.res");
    assert_eq!(use_.len(), 1);
    assert_eq!(use_[0].text, "Palette.color");
}

// -------------------------------------------------------------------------------------------
// The crux (SPEC §3.2): unqualified resolution through `open`, aliases, and shadowing.
// -------------------------------------------------------------------------------------------

#[test]
fn open_makes_a_bare_identifier_a_reference_and_no_open_does_not() {
    let proj = TempProject::new(&[
        ("Types.res", "let zero = 0\n"),
        ("Opened.res", "open Types\nlet a = zero\n"),
        ("Closed.res", "let zero = 5\nlet b = zero\n"),
    ]);
    let rs = proj.refs("Types.res", &["zero"]);

    let opened = in_file(&rs, "Opened.res");
    assert_eq!(opened.len(), 1, "got {rs:#?}");
    assert_eq!(opened[0].kind, RefKind::UnqualifiedViaOpen);
    assert_eq!(opened[0].line, 2);

    assert!(
        in_file(&rs, "Closed.res").is_empty(),
        "without an `open`, a bare `zero` is a different `zero` entirely"
    );
}

#[test]
fn an_alias_is_reported_as_via_alias() {
    let proj = TempProject::new(&[
        ("Types.res", "let zero = 0\n"),
        ("User.res", "module T = Types\nlet a = T.zero\n"),
    ]);
    let rs = proj.refs("Types.res", &["zero"]);
    let user = in_file(&rs, "User.res");
    assert_eq!(user.len(), 1);
    assert_eq!(user[0].kind, RefKind::ViaAlias);
    assert_eq!(user[0].text, "T.zero");
}

#[test]
fn include_brings_names_into_unqualified_scope_like_open() {
    // `include Base` splices Base's contents in, so a bare `shared` resolves to `Base.shared`.
    // (The *re-export* — `Ext.shared` from a third file — is a documented non-goal.)
    let proj = TempProject::new(&[
        ("Base.res", "let shared = 1\n"),
        (
            "Ext.res",
            "include Base\nlet doubled = shared * 2\nlet viaModule = Base.shared\n",
        ),
    ]);
    let rs = proj.refs("Base.res", &["shared"]);
    let ext = in_file(&rs, "Ext.res");
    assert_eq!(ext.len(), 2, "got {rs:#?}");
    assert_eq!(ext[0].kind, RefKind::UnqualifiedViaOpen);
    assert_eq!(ext[0].line, 2);
    assert_eq!(ext[1].kind, RefKind::Qualified);
    assert_eq!(ext[1].line, 3);
}

#[test]
fn the_fixture_alias_makes_belt_array_reachable() {
    // Main.res has `module Arr = Belt.Array`; `refs` on a project module named `Arr` must not
    // confuse the alias's *binding* name with a reference to it.
    let rs = refs(MAIN, &[]);
    assert!(
        rs.iter().all(|r| r.kind == RefKind::Definition),
        "nothing in the fixture references module Main: {rs:#?}"
    );
}

#[test]
fn a_shadowed_bare_name_is_labelled_rather_than_dropped() {
    // Under-reporting is the dangerous direction, so a locally-shadowed occurrence is still
    // emitted — with a classification that says it probably is not a real reference.
    let proj = TempProject::new(&[
        ("Types.res", "let zero = 0\n"),
        (
            "User.res",
            "open Types\nlet f = () => {\n  let zero = 99\n  zero\n}\nlet g = zero\n",
        ),
    ]);
    let rs = proj.refs("Types.res", &["zero"]);
    let user = in_file(&rs, "User.res");

    let shadowed: Vec<&&Reference> = user
        .iter()
        .filter(|r| r.kind == RefKind::UnqualifiedShadowed)
        .collect();
    assert_eq!(shadowed.len(), 1, "got {user:#?}");
    assert_eq!(shadowed[0].line, 4, "the `zero` under the local binder");

    let live: Vec<&&Reference> = user
        .iter()
        .filter(|r| r.kind == RefKind::UnqualifiedViaOpen)
        .collect();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].line, 6, "`let g = zero` is a genuine reference");
}

#[test]
fn a_local_open_does_not_leak_past_its_block() {
    let proj = TempProject::new(&[
        ("Types.res", "let zero = 0\n"),
        (
            "User.res",
            "let f = () => {\n  open Types\n  zero\n}\nlet g = zero\n",
        ),
    ]);
    let rs = proj.refs("Types.res", &["zero"]);
    let user = in_file(&rs, "User.res");
    assert_eq!(user.len(), 1, "only the in-block `zero`, got {user:#?}");
    assert_eq!(user[0].line, 3);
}

#[test]
fn a_partially_qualified_path_under_an_open_is_found() {
    // `open Host` makes `Inner.helper` a spelling of `Host.Inner.helper`.
    let proj = TempProject::new(&[
        ("Host.res", "module Inner = {\n  let helper = 1\n}\n"),
        ("User.res", "open Host\nlet a = Inner.helper\n"),
    ]);
    let rs = proj.refs("Host.res", &["Inner.helper"]);
    let user = in_file(&rs, "User.res");
    assert_eq!(user.len(), 1, "got {rs:#?}");
    assert_eq!(user[0].kind, RefKind::UnqualifiedViaOpen);
    assert_eq!(user[0].text, "Inner.helper");
}

#[test]
fn a_declarations_own_binder_elsewhere_is_not_a_reference() {
    // Another module declaring its own `zero` must not be reported — that is a definition of a
    // different thing, and reporting it would make every common name unusable.
    let proj = TempProject::new(&[
        ("Types.res", "let zero = 0\n"),
        ("Other.res", "open Types\nlet zero = 1\n"),
    ]);
    let rs = proj.refs("Types.res", &["zero"]);
    assert!(
        in_file(&rs, "Other.res").is_empty(),
        "`let zero = 1` binds a name, it does not reference one: {rs:#?}"
    );
}

#[test]
fn record_field_names_are_never_mistaken_for_references() {
    // SPEC §1 finding 8 on the expression side: `{name: 1}`'s `name` is a field, and a
    // `record_pattern` binder is a binder. Neither is a reference to `Fields.name`.
    let proj = TempProject::new(&[
        ("Fields.res", "let name = \"n\"\n"),
        (
            "User.res",
            "open Fields\nlet r = {name: 1}\nlet {name: alias} = r\nlet direct = name\n",
        ),
    ]);
    let rs = proj.refs("Fields.res", &["name"]);
    let user = in_file(&rs, "User.res");
    assert_eq!(
        user.len(),
        1,
        "only `let direct = name` is a reference, got {user:#?}"
    );
    assert_eq!(user[0].line, 4);
}

// -------------------------------------------------------------------------------------------
// Project config: namespacing, subdirectories, and file discovery.
// -------------------------------------------------------------------------------------------

#[test]
fn a_namespaced_project_matches_both_spellings() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("rescript.json"),
        r#"{ "name": "my-app", "namespace": true, "sources": [{ "dir": "src", "subdirs": true }] }"#,
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/Types.res"), "let zero = 0\n").unwrap();
    std::fs::write(
        dir.path().join("src/Use.res"),
        "let a = MyApp.Types.zero\nlet b = Types.zero\n",
    )
    .unwrap();

    let rs = find(&dir.path().join("src/Types.res"), &["zero".to_string()]).unwrap();
    let use_ = in_file(&rs, "Use.res");
    assert_eq!(
        use_.len(),
        2,
        "both `MyApp.Types.zero` and `Types.zero` name the same declaration: {rs:#?}"
    );
    assert!(use_.iter().all(|r| r.kind == RefKind::Qualified));
}

#[test]
fn a_module_in_a_subdirectory_is_addressed_by_basename_alone() {
    // SPEC §3.2: `src/nested/Util.res` is module `Util`, not `Nested.Util`.
    let rs = refs(UTIL, &[]);
    let def = rs
        .iter()
        .find(|r| r.kind == RefKind::Definition)
        .expect("Util.res defines module Util");
    assert_eq!(def.text, "Util");
    assert!(def.file.ends_with("nested/Util.res"));
}

#[test]
fn subdirectory_files_are_scanned_for_references() {
    let proj = TempProject::new(&[
        ("Types.res", "let zero = 0\n"),
        ("deep/Buried.res", "let a = Types.zero\n"),
    ]);
    let rs = proj.refs("Types.res", &["zero"]);
    assert_eq!(in_file(&rs, "Buried.res").len(), 1, "got {rs:#?}");
}

// -------------------------------------------------------------------------------------------
// Output plumbing.
// -------------------------------------------------------------------------------------------

#[test]
fn both_output_formats_run_cleanly() {
    assert!(
        run(
            PathBuf::from(TYPES),
            vec!["msg".to_string()],
            Format::Compact
        )
        .is_ok()
    );
    assert!(run(PathBuf::from(TYPES), vec!["msg".to_string()], Format::Json).is_ok());
}

#[test]
fn several_targets_can_be_queried_at_once() {
    let rs = refs(TYPES, &["msg", "user"]);
    assert!(rs.iter().any(|r| r.target == "Types.msg"));
    assert!(rs.iter().any(|r| r.target == "Types.user"));
}

#[test]
fn results_are_ordered_by_file_then_position() {
    let rs = refs(TYPES, &[]);
    let keys: Vec<(PathBuf, usize)> = rs.iter().map(|r| (r.file.clone(), r.byte)).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted, "output must be deterministic and ordered");
}

#[test]
fn an_unparseable_file_does_not_abort_the_run() {
    // `refs` is a read command (SPEC §2) — tolerant. A file resq cannot parse must warn and be
    // skipped, never take the whole query down with it.
    let proj = TempProject::new(&[
        ("Types.res", "let zero = 0\n"),
        ("Good.res", "let a = Types.zero\n"),
        ("Broken.res", "let = = = \nmodule {{{\n"),
    ]);
    let rs = proj.refs("Types.res", &["zero"]);
    assert_eq!(
        in_file(&rs, "Good.res").len(),
        1,
        "a broken sibling must not hide a good file's references: {rs:#?}"
    );
}
