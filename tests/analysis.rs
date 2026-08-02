//! Wave 2 (agent A2) — tests for `resq list`: `src/analysis.rs`.
//!
//! Exercises `extract_summary` / `render_compact` / `to_json_summary` directly against
//! `tests/fixtures/proj/` and `tests/fixtures/broken.res`, plus a couple of end-to-end
//! invocations of the built binary for the parts of the contract that are only observable at the
//! process boundary (stderr warning + exit code, and the shape of real CLI stdout).

use resq::analysis::{extract_summary, render_compact, run_list, to_json_summary};
use resq::cli::Format;
use resq::parser::parse;
use resq::{DeclarationKind, FileSummary, ModulePath, module_name_from_path};
use std::fs;
use std::path::Path;
use std::process::Command;

const MAIN: &str = "tests/fixtures/proj/src/Main.res";
const TYPES: &str = "tests/fixtures/proj/src/Types.res";
const MODERN: &str = "tests/fixtures/proj/src/Modern.res";
const BROKEN: &str = "tests/fixtures/broken.res";

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

fn summary_for(path: &str) -> FileSummary {
    let src = read(path);
    let tree = parse(&src).expect("parses");
    let module_name = module_name_from_path(Path::new(path));
    extract_summary(&tree, &src, module_name)
}

// -------------------------------------------------------------------------------------------
// 1. Main.res: entry, update, the destructuring binding, the external, and nested Inner /
//    Inner.Deep at the right depths.
// -------------------------------------------------------------------------------------------

#[test]
fn main_res_lists_top_level_and_nested_declarations() {
    let summary = summary_for(MAIN);

    let entry = summary
        .find_declaration(&ModulePath::parse("entry"))
        .expect("entry at file root");
    assert_eq!(entry.kind, DeclarationKind::Let);
    assert_eq!(entry.path, ModulePath::root());
    assert_eq!(
        entry.doc_comment.as_deref(),
        Some("/** Top-level entry point. */")
    );
    assert_eq!(entry.decorators, vec!["@genType".to_string()]);

    let update = summary
        .find_declaration(&ModulePath::parse("update"))
        .expect("update at file root");
    assert_eq!(update.kind, DeclarationKind::Let);
    assert_eq!(update.start_line, 12);
    assert_eq!(update.end_line, 18);

    // `let (first, second) = (1, 2)` — one binding, two names, destructuring.
    let destructured = summary
        .find_declaration(&ModulePath::parse("first"))
        .expect("`first` is bound by the destructuring let");
    assert_eq!(
        destructured.names,
        vec!["first".to_string(), "second".to_string()]
    );
    assert_eq!(destructured.binder_kind, resq::BinderKind::Destructuring);
    assert!(
        summary
            .find_declaration(&ModulePath::parse("second"))
            .is_some(),
        "`second` must resolve to the same destructuring declaration"
    );

    let external = summary
        .find_declaration(&ModulePath::parse("evalRaw"))
        .expect("evalRaw external");
    assert_eq!(external.kind, DeclarationKind::External);
    assert_eq!(external.type_annotation.as_deref(), Some("string => unit"));

    // Nested module depth: `Inner` at the root, `Inner.helper` one deep, `Inner.Deep` one deep,
    // `Inner.Deep.deepValue` two deep.
    let inner = summary
        .find_declaration(&ModulePath::parse("Inner"))
        .expect("Inner module declaration");
    assert_eq!(inner.kind, DeclarationKind::Module);
    assert_eq!(inner.path, ModulePath::root());

    let helper = summary
        .find_declaration(&ModulePath::parse("Inner.helper"))
        .expect("Inner.helper");
    assert_eq!(helper.path, ModulePath::parse("Inner"));
    assert_eq!(helper.doc_comment.as_deref(), Some("/** Nested helper. */"));

    let deep = summary
        .find_declaration(&ModulePath::parse("Inner.Deep"))
        .expect("Inner.Deep module declaration");
    assert_eq!(deep.kind, DeclarationKind::Module);
    assert_eq!(deep.path, ModulePath::parse("Inner"));

    let deep_value = summary
        .find_declaration(&ModulePath::parse("Inner.Deep.deepValue"))
        .expect("Inner.Deep.deepValue");
    assert_eq!(deep_value.path, ModulePath::parse("Inner.Deep"));

    // A module's own declaration precedes its members, in source order.
    let inner_idx = summary
        .declarations
        .iter()
        .position(|d| d.is_at(&ModulePath::parse("Inner")))
        .unwrap();
    let helper_idx = summary
        .declarations
        .iter()
        .position(|d| d.is_at(&ModulePath::parse("Inner.helper")))
        .unwrap();
    let deep_idx = summary
        .declarations
        .iter()
        .position(|d| d.is_at(&ModulePath::parse("Inner.Deep")))
        .unwrap();
    let deep_value_idx = summary
        .declarations
        .iter()
        .position(|d| d.is_at(&ModulePath::parse("Inner.Deep.deepValue")))
        .unwrap();
    assert!(inner_idx < helper_idx);
    assert!(helper_idx < deep_idx);
    assert!(deep_idx < deep_value_idx);
}

#[test]
fn main_res_opens_and_aliases_are_not_double_counted_as_declarations() {
    let summary = summary_for(MAIN);

    assert_eq!(summary.opens, vec!["Belt".to_string()]);
    assert_eq!(summary.aliases.len(), 1);
    assert_eq!(summary.aliases[0].name, "Arr");
    assert_eq!(summary.aliases[0].target, "Belt.Array");

    // The alias must not also show up as an addressable Module declaration — it has no members
    // and the compact render must not show it twice (once under `aliases:`, once under
    // `modules:`).
    assert!(
        summary
            .find_declaration(&ModulePath::parse("Arr"))
            .is_none()
    );
    assert!(
        summary
            .declarations
            .iter()
            .all(|d| d.kind != DeclarationKind::Open),
        "open statements should not be duplicated into the flat declaration list"
    );
}

// -------------------------------------------------------------------------------------------
// 2. Types.res: msg, user, id, abstractThing are all reported as types.
// -------------------------------------------------------------------------------------------

#[test]
fn types_res_reports_all_type_declarations() {
    let summary = summary_for(TYPES);
    let type_names: Vec<&str> = summary
        .declarations
        .iter()
        .filter(|d| d.kind == DeclarationKind::Type)
        .flat_map(|d| d.names.iter().map(String::as_str))
        .collect();
    assert_eq!(type_names, vec!["msg", "user", "id", "abstractThing"]);
}

// -------------------------------------------------------------------------------------------
// 3. Modern.res: `let rec even = ... and odd = ...` is ONE declaration binding both names
//    (SPEC §1 finding 6). This is the easiest thing to get wrong.
// -------------------------------------------------------------------------------------------

#[test]
fn modern_res_let_rec_and_binds_both_names_in_one_declaration() {
    let summary = summary_for(MODERN);

    let even = summary
        .declarations
        .iter()
        .find(|d| d.names.iter().any(|n| n == "even"))
        .expect("even");
    assert_eq!(even.names, vec!["even".to_string(), "odd".to_string()]);
    assert_eq!(even.binder_kind, resq::BinderKind::Simple);
    assert_eq!(even.start_line, 14);
    assert_eq!(even.end_line, 15);

    // `even` and `odd` must resolve to the exact same Declaration, not two separate ones.
    let odd = summary
        .find_declaration(&ModulePath::parse("odd"))
        .expect("odd resolves");
    assert_eq!(odd as *const _, even as *const _);

    // Only one `let` declaration total binds either name.
    let count = summary
        .declarations
        .iter()
        .filter(|d| d.names.iter().any(|n| n == "even" || n == "odd"))
        .count();
    assert_eq!(count, 1, "even/odd must be a single declaration, not two");
}

// -------------------------------------------------------------------------------------------
// 4. broken.res: warns on stderr, still prints the summary for well-formed portions, exits 0.
// -------------------------------------------------------------------------------------------

#[test]
fn broken_res_extraction_does_not_panic_and_reports_no_declarations() {
    // analysis.rs must not call `ensure_clean_parse` — a raw `parse` on a badly broken file, fed
    // straight into `extract_summary`, must not panic and should simply yield nothing recognized.
    let src = read(BROKEN);
    let tree = parse(&src).expect("tree-sitter always returns *a* tree, error nodes and all");
    assert!(tree.root_node().has_error());
    let summary = extract_summary(&tree, &src, "Broken".to_string());
    assert!(summary.declarations.is_empty());

    // render_compact must not panic on an empty summary either.
    let rendered = render_compact(&summary, src.lines().count().max(1), false);
    assert!(rendered.starts_with("module Broken"));
}

#[test]
fn broken_res_cli_warns_on_stderr_prints_summary_and_exits_zero() {
    let exe = env!("CARGO_BIN_EXE_resq");
    let output = Command::new(exe)
        .args(["list", BROKEN])
        .output()
        .expect("failed to run resq");

    assert!(
        output.status.success(),
        "list on a broken file must exit 0, got {:?}",
        output.status
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.to_lowercase().contains("warn"),
        "expected a warning on stderr, got: {stderr}"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("module Broken"),
        "the summary must still print for the well-formed portions, got: {stdout}"
    );
}

#[test]
fn run_list_itself_never_errors_on_a_readable_but_broken_file() {
    // Exercises the public `run_list` entry point in-process (not just via the binary), matching
    // what `main.rs` calls.
    let result = run_list(
        vec![Path::new(BROKEN).to_path_buf()],
        Format::Compact,
        false,
    );
    assert!(
        result.is_ok(),
        "list must exit 0 on a parse-broken file: {result:?}"
    );
}

// -------------------------------------------------------------------------------------------
// 5. `--format json` round-trips to valid JSON with full dot-paths for nested declarations.
// -------------------------------------------------------------------------------------------

#[test]
fn json_format_round_trips_with_full_dot_paths() {
    let summary = summary_for(MAIN);
    let value = to_json_summary(&summary);
    let text = serde_json::to_string_pretty(&value).expect("serializes");

    // Round-trip: what we just printed must itself parse back as valid JSON.
    let reparsed: serde_json::Value = serde_json::from_str(&text).expect("round-trips as JSON");
    assert_eq!(reparsed, value);

    let declarations = value["declarations"]
        .as_array()
        .expect("declarations array");
    let paths_for = |name: &str| -> Vec<String> {
        declarations
            .iter()
            .find(|d| {
                d["paths"]
                    .as_array()
                    .is_some_and(|ps| ps.iter().any(|p| p.as_str() == Some(name)))
            })
            .map(|d| {
                d["paths"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| p.as_str().unwrap().to_string())
                    .collect()
            })
            .unwrap_or_else(|| panic!("no declaration with path `{name}` in {text}"))
    };

    // Nested declarations must carry their FULL dot-path, not just the enclosing path.
    assert_eq!(paths_for("Inner.helper"), vec!["Inner.helper".to_string()]);
    assert_eq!(
        paths_for("Inner.Deep.deepValue"),
        vec!["Inner.Deep.deepValue".to_string()]
    );

    // A destructuring binding reports every name it binds, each with its own full path.
    let first_decl = declarations
        .iter()
        .find(|d| {
            d["paths"]
                .as_array()
                .is_some_and(|ps| ps.iter().any(|p| p.as_str() == Some("first")))
        })
        .expect("destructuring declaration");
    let paths: Vec<&str> = first_decl["paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    assert_eq!(paths, vec!["first", "second"]);

    // The module_name and opens round-trip too.
    assert_eq!(value["module_name"], "Main");
    assert_eq!(value["opens"][0], "Belt");
    assert_eq!(value["aliases"][0]["name"], "Arr");
    assert_eq!(value["aliases"][0]["target"], "Belt.Array");
}

#[test]
fn json_format_cli_round_trip_end_to_end() {
    let exe = env!("CARGO_BIN_EXE_resq");
    let output = Command::new(exe)
        .args(["list", MAIN, "--format", "json"])
        .output()
        .expect("failed to run resq");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout was not valid JSON ({e}): {stdout}"));
    assert_eq!(value["module_name"], "Main");
}

// -------------------------------------------------------------------------------------------
// Compact rendering shape (SPEC A2 task): grouping, indentation by depth, --docs.
// -------------------------------------------------------------------------------------------

#[test]
fn compact_render_groups_by_kind_and_indents_nested_modules_by_depth() {
    let summary = summary_for(MAIN);
    let total_lines = read(MAIN).lines().count();
    let rendered = render_compact(&summary, total_lines, false);

    assert!(rendered.starts_with(&format!("module Main  ({total_lines} lines)\n")));
    assert!(rendered.contains("opens:\n  Belt\n"));
    assert!(rendered.contains("aliases:\n  Arr = Belt.Array\n"));
    assert!(rendered.contains("functions:\n"));
    assert!(rendered.contains("externals:\n  evalRaw"));
    assert!(rendered.contains("modules:\n  Inner"));

    // Depth 1 (module Inner): 2-space indent. Depth 2 (Inner's members): 4-space indent.
    // Depth 3 (Inner.Deep's members): 6-space indent.
    assert!(rendered.contains("\n  Inner "));
    assert!(rendered.contains("\n    helper "));
    assert!(rendered.contains("\n    Deep "));
    assert!(rendered.contains("\n      deepValue "));

    // The alias must not be rendered a second time under `modules:`.
    let modules_section = rendered.split("modules:").nth(1).unwrap();
    assert!(!modules_section.contains("Arr"));
}

#[test]
fn compact_render_docs_flag_prints_doc_comments_indented_beneath() {
    let summary = summary_for(MAIN);
    let rendered = render_compact(&summary, read(MAIN).lines().count(), true);
    assert!(rendered.contains("entry"));
    let entry_pos = rendered.find("entry").unwrap();
    let after_entry = &rendered[entry_pos..];
    assert!(
        after_entry
            .lines()
            .take(2)
            .nth(1)
            .unwrap()
            .trim()
            .starts_with("/** Top-level entry point. */")
    );
}

#[test]
fn multiple_files_each_get_a_summary() {
    let exe = env!("CARGO_BIN_EXE_resq");
    let output = Command::new(exe)
        .args(["list", MAIN, TYPES])
        .output()
        .expect("failed to run resq");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("module Main"));
    assert!(stdout.contains("module Types"));
}
