//! Wave 2 (agent A8) — tests for `resq add open`, `resq add alias`, `resq rm open`:
//! `src/imports.rs`.
//!
//! Every write command here is exercised against a copy of a fixture in a `tempfile::TempDir` —
//! never against `tests/fixtures/` directly (SPEC §2 / task instructions). `tests/fixtures/`
//! stays byte-for-byte as the conductor built it.

use resq::cli::{AddAlias, AddOpen, RmOpen};
use resq::imports::{run_add_alias, run_add_open, run_rm_open};
use resq::parser;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const MAIN: &str = "tests/fixtures/proj/src/Main.res";
const TYPES: &str = "tests/fixtures/proj/src/Types.res";
const BROKEN: &str = "tests/fixtures/broken.res";

/// Copy a fixture into a fresh temp dir under its own basename, returning the temp dir (kept
/// alive by the caller) and the copied file's path.
fn copy_fixture(fixture: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let name = Path::new(fixture).file_name().unwrap();
    let dest = dir.path().join(name);
    fs::copy(fixture, &dest)
        .unwrap_or_else(|e| panic!("failed to copy fixture {fixture} to {dest:?}: {e}"));
    (dir, dest)
}

fn write_temp(contents: &str, name: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join(name);
    fs::write(&dest, contents).unwrap();
    (dir, dest)
}

fn reparses_clean(src: &str) -> bool {
    !parser::parse(src)
        .expect("tree-sitter always returns a tree")
        .root_node()
        .has_error()
}

// -------------------------------------------------------------------------------------------
// 1. `add open` on a copy of Main.res inserts correctly and the result re-parses clean.
// -------------------------------------------------------------------------------------------

#[test]
fn add_open_inserts_new_module_and_result_reparses_clean() {
    let (_dir, file) = copy_fixture(MAIN);

    run_add_open(AddOpen {
        file: file.clone(),
        modules: vec!["Js.Console".to_string()],
    })
    .expect("add open should succeed");

    let after = fs::read_to_string(&file).unwrap();
    assert!(
        after.lines().any(|l| l == "open Js.Console"),
        "expected `open Js.Console` inserted, got:\n{after}"
    );
    // Inserted near the existing opens/aliases at the top, not at some arbitrary point.
    let open_belt_idx = after.lines().position(|l| l == "open Belt").unwrap();
    let alias_idx = after
        .lines()
        .position(|l| l == "module Arr = Belt.Array")
        .unwrap();
    let new_open_idx = after.lines().position(|l| l == "open Js.Console").unwrap();
    assert!(open_belt_idx < new_open_idx);
    assert!(alias_idx < new_open_idx);

    assert!(
        reparses_clean(&after),
        "result must re-parse clean:\n{after}"
    );
}

/// `add open` with no existing opens/aliases inserts at the top, after (not swallowing) the
/// leading doc comment that is attached to the first declaration — Types.res's `/** The
/// application message type. */` belongs to `type msg`, not to the file, so it must not be
/// mistaken for a file-level doc comment.
#[test]
fn add_open_with_no_existing_opens_inserts_before_attached_leading_doc_comment() {
    let (_dir, file) = copy_fixture(TYPES);

    run_add_open(AddOpen {
        file: file.clone(),
        modules: vec!["Js.Console".to_string()],
    })
    .expect("add open should succeed");

    let after = fs::read_to_string(&file).unwrap();
    assert!(after.starts_with("open Js.Console\n"), "got:\n{after}");
    assert!(reparses_clean(&after));
}

// -------------------------------------------------------------------------------------------
// 2. `add open` of an already-present module is a no-op, exit 0, file untouched.
// -------------------------------------------------------------------------------------------

#[test]
fn add_open_of_already_present_module_is_a_noop() {
    let (_dir, file) = copy_fixture(MAIN);
    let before = fs::read_to_string(&file).unwrap();

    let result = run_add_open(AddOpen {
        file: file.clone(),
        modules: vec!["Belt".to_string()],
    });
    assert!(result.is_ok(), "no-op must still exit 0: {result:?}");

    let after = fs::read_to_string(&file).unwrap();
    assert_eq!(before, after, "no-op must not touch the file at all");
}

// -------------------------------------------------------------------------------------------
// 3. `add alias Arr2=Belt.Array` produces `module Arr2 = Belt.Array` that re-parses clean.
// -------------------------------------------------------------------------------------------

#[test]
fn add_alias_arr2_belt_array_reparses_clean() {
    let (_dir, file) = copy_fixture(MAIN);

    run_add_alias(AddAlias {
        file: file.clone(),
        aliases: vec!["Arr2=Belt.Array".to_string()],
    })
    .expect("add alias should succeed");

    let after = fs::read_to_string(&file).unwrap();
    assert!(
        after.lines().any(|l| l == "module Arr2 = Belt.Array"),
        "got:\n{after}"
    );
    assert!(reparses_clean(&after));
}

#[test]
fn add_alias_is_a_noop_when_the_exact_pair_already_exists() {
    let (_dir, file) = copy_fixture(MAIN);
    let before = fs::read_to_string(&file).unwrap();

    let result = run_add_alias(AddAlias {
        file: file.clone(),
        aliases: vec!["Arr=Belt.Array".to_string()],
    });
    assert!(result.is_ok());

    let after = fs::read_to_string(&file).unwrap();
    assert_eq!(
        before, after,
        "identical existing alias must be a true no-op"
    );
}

// -------------------------------------------------------------------------------------------
// 4. `rm open Belt` on Main.res — DELIBERATE DECISION, explained in the test name: Main.res
//    never references anything unqualified anywhere in the file (every value use is either a
//    local binding or explicitly qualified, e.g. `Types.Increment`), so the free-identifier scan
//    finds nothing and removal proceeds without needing --force. This is the heuristic's clean
//    "provably safe" case (see the module doc comment on src/imports.rs for the exact contract).
// -------------------------------------------------------------------------------------------

#[test]
fn rm_open_belt_on_main_succeeds_because_the_file_has_zero_unqualified_references() {
    let (_dir, file) = copy_fixture(MAIN);

    run_rm_open(RmOpen {
        file: file.clone(),
        modules: vec!["Belt".to_string()],
        force: false,
    })
    .expect("rm open must succeed: Main.res has no unqualified references at all");

    let after = fs::read_to_string(&file).unwrap();
    assert!(!after.lines().any(|l| l == "open Belt"), "got:\n{after}");
    // No blank line left behind where the `open` used to be.
    assert!(
        !after.starts_with('\n'),
        "removal must not leave a leading blank line:\n{after}"
    );
    assert!(reparses_clean(&after));
}

/// The mirror image of the above, in one file: an `open` whose removal the heuristic MUST
/// refuse, because the file contains a genuinely free (unbound, unqualified) identifier that
/// resq cannot prove is unrelated to `Belt`.
#[test]
fn rm_open_refuses_when_file_has_an_unqualified_free_identifier() {
    let (_dir, file) = write_temp(
        "open Belt\nlet useIt = () => totallyUnboundName\n",
        "Risky.res",
    );
    let before = fs::read_to_string(&file).unwrap();

    let result = run_rm_open(RmOpen {
        file: file.clone(),
        modules: vec!["Belt".to_string()],
        force: false,
    });
    assert!(result.is_err(), "must refuse: {result:?}");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("totallyUnboundName"),
        "error should name the candidate: {msg}"
    );
    assert!(msg.contains("--force"));

    let after = fs::read_to_string(&file).unwrap();
    assert_eq!(before, after, "a refused rm open must not touch the file");
}

/// Regression test for the exact cross-scope masking bug the coordinator reported: a `let map`
/// bound *inside* `f`'s own body must not hide the genuinely free `map` referenced by the
/// sibling `g`, which (in real code) resolves through `open Belt`. Before the scope-aware rewrite
/// of `scan_free_identifiers`, `f`'s local `map` was added to one whole-file `bound` set, so `g`'s
/// `map` was (wrongly) treated as bound and `rm open Belt` silently removed the open and left `g`
/// referring to nothing — the dangerous, under-approximating direction the heuristic must not
/// take. The scope stack in `src/imports.rs` pops `f`'s frame before `g` is ever visited, so `g`'s
/// `map` is checked against only the frames actually enclosing it.
#[test]
fn rm_open_refuses_when_a_same_named_local_in_a_different_function_would_have_masked_the_use() {
    let (_dir, file) = write_temp(
        "open Belt\n\nlet f = () => {\n  let map = 1\n  map\n}\n\nlet g = () => map\n",
        "Scoping.res",
    );
    let before = fs::read_to_string(&file).unwrap();

    let result = run_rm_open(RmOpen {
        file: file.clone(),
        modules: vec!["Belt".to_string()],
        force: false,
    });
    assert!(
        result.is_err(),
        "f's local `map` must not mask g's free `map`: {result:?}"
    );
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("map"),
        "error should name the free `map`: {msg}"
    );

    let after = fs::read_to_string(&file).unwrap();
    assert_eq!(before, after, "a refused rm open must not touch the file");

    // --force still removes it regardless.
    run_rm_open(RmOpen {
        file: file.clone(),
        modules: vec!["Belt".to_string()],
        force: true,
    })
    .expect("--force must bypass the safety scan");
    let forced = fs::read_to_string(&file).unwrap();
    assert!(!forced.lines().any(|l| l == "open Belt"), "got:\n{forced}");
}

/// A `let` bound inside one function's body IS visible to code later in that *same* function —
/// scope-awareness must not make the heuristic more conservative than it needs to be for the
/// ordinary, safe case of a local variable used only where it's actually in scope.
#[test]
fn rm_open_still_succeeds_when_a_local_binding_is_only_used_within_its_own_function() {
    let (_dir, file) = write_temp(
        "open Belt\n\nlet f = () => {\n  let map = 1\n  map\n}\n",
        "Local.res",
    );

    run_rm_open(RmOpen {
        file: file.clone(),
        modules: vec!["Belt".to_string()],
        force: false,
    })
    .expect("a local binding used only inside its own function must not trigger a refusal");
}

// -------------------------------------------------------------------------------------------
// 5. `rm open --force` removes regardless of the safety scan.
// -------------------------------------------------------------------------------------------

#[test]
fn rm_open_force_removes_despite_unqualified_reference() {
    let (_dir, file) = write_temp(
        "open Belt\nlet useIt = () => totallyUnboundName\n",
        "Risky.res",
    );

    run_rm_open(RmOpen {
        file: file.clone(),
        modules: vec!["Belt".to_string()],
        force: true,
    })
    .expect("--force must bypass the safety scan");

    let after = fs::read_to_string(&file).unwrap();
    assert!(!after.lines().any(|l| l == "open Belt"), "got:\n{after}");
}

// -------------------------------------------------------------------------------------------
// 6. Every write command refuses on tests/fixtures/broken.res and writes zero bytes.
// -------------------------------------------------------------------------------------------

#[test]
fn add_open_refuses_on_broken_res_and_leaves_it_byte_identical() {
    let (_dir, file) = copy_fixture(BROKEN);
    let before = fs::read(&file).unwrap();

    let result = run_add_open(AddOpen {
        file: file.clone(),
        modules: vec!["Belt".to_string()],
    });
    assert!(
        result.is_err(),
        "must refuse on a pre-broken file: {result:?}"
    );

    let after = fs::read(&file).unwrap();
    assert_eq!(
        before, after,
        "broken.res must be byte-identical after a refused write"
    );
}

#[test]
fn add_alias_refuses_on_broken_res_and_leaves_it_byte_identical() {
    let (_dir, file) = copy_fixture(BROKEN);
    let before = fs::read(&file).unwrap();

    let result = run_add_alias(AddAlias {
        file: file.clone(),
        aliases: vec!["Arr2=Belt.Array".to_string()],
    });
    assert!(
        result.is_err(),
        "must refuse on a pre-broken file: {result:?}"
    );

    let after = fs::read(&file).unwrap();
    assert_eq!(
        before, after,
        "broken.res must be byte-identical after a refused write"
    );
}

#[test]
fn rm_open_refuses_on_broken_res_and_leaves_it_byte_identical() {
    let (_dir, file) = copy_fixture(BROKEN);
    let before = fs::read(&file).unwrap();

    // broken.res has no `open` at all, but ensure_clean_parse (step 1) must reject it before
    // imports.rs ever gets to looking for one.
    let result = run_rm_open(RmOpen {
        file: file.clone(),
        modules: vec!["Belt".to_string()],
        force: false,
    });
    assert!(
        result.is_err(),
        "must refuse on a pre-broken file: {result:?}"
    );

    let after = fs::read(&file).unwrap();
    assert_eq!(
        before, after,
        "broken.res must be byte-identical after a refused write"
    );
}

#[test]
fn rm_open_force_also_refuses_on_broken_res_step_1_precedes_the_safety_scan() {
    let (_dir, file) = copy_fixture(BROKEN);
    let before = fs::read(&file).unwrap();

    let result = run_rm_open(RmOpen {
        file: file.clone(),
        modules: vec!["Belt".to_string()],
        force: true,
    });
    assert!(
        result.is_err(),
        "--force skips the *safety scan*, not step 1 (ensure_clean_parse): {result:?}"
    );

    let after = fs::read(&file).unwrap();
    assert_eq!(before, after);
}

// -------------------------------------------------------------------------------------------
// 7. A deliberately malformed module name is rejected by output validation, file unchanged.
// -------------------------------------------------------------------------------------------

#[test]
fn add_open_with_malformed_module_name_is_rejected_by_output_validation() {
    let (_dir, file) = copy_fixture(MAIN);
    let before = fs::read_to_string(&file).unwrap();

    // A space makes this impossible to parse as a module path — `ensure_clean_parse` on the
    // *input* still succeeds (Main.res itself is fine); it's the *output* buffer that fails to
    // re-parse, which is exactly what `writer::validate_output` exists to catch.
    let result = run_add_open(AddOpen {
        file: file.clone(),
        modules: vec!["not a module".to_string()],
    });
    assert!(
        result.is_err(),
        "malformed module name must be rejected: {result:?}"
    );
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("does not parse") || msg.contains("unchanged"),
        "expected an output-validation-shaped error, got: {msg}"
    );

    let after = fs::read_to_string(&file).unwrap();
    assert_eq!(before, after, "rejected output must never be written");
}

#[test]
fn add_alias_with_malformed_target_is_rejected_by_output_validation() {
    let (_dir, file) = copy_fixture(MAIN);
    let before = fs::read_to_string(&file).unwrap();

    let result = run_add_alias(AddAlias {
        file: file.clone(),
        aliases: vec!["Bad=not a module".to_string()],
    });
    assert!(
        result.is_err(),
        "malformed alias target must be rejected: {result:?}"
    );

    let after = fs::read_to_string(&file).unwrap();
    assert_eq!(before, after);
}

#[test]
fn add_alias_argument_without_equals_is_rejected_before_touching_the_file() {
    let (_dir, file) = copy_fixture(MAIN);
    let before = fs::read_to_string(&file).unwrap();

    let result = run_add_alias(AddAlias {
        file: file.clone(),
        aliases: vec!["NoEqualsSign".to_string()],
    });
    assert!(result.is_err());

    let after = fs::read_to_string(&file).unwrap();
    assert_eq!(before, after);
}

// -------------------------------------------------------------------------------------------
// End-to-end sanity through the built binary.
// -------------------------------------------------------------------------------------------

#[test]
fn end_to_end_add_open_via_binary_prints_ok() {
    let (_dir, file) = copy_fixture(MAIN);
    let exe = env!("CARGO_BIN_EXE_resq");
    let output = Command::new(exe)
        .args(["add", "open", file.to_str().unwrap(), "Js.Console"])
        .output()
        .expect("failed to run resq");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "ok");
}

#[test]
fn end_to_end_rm_open_without_force_exits_nonzero_on_a_risky_file() {
    let (_dir, file) = write_temp(
        "open Belt\nlet useIt = () => totallyUnboundName\n",
        "Risky.res",
    );
    let exe = env!("CARGO_BIN_EXE_resq");
    let output = Command::new(exe)
        .args(["rm", "open", file.to_str().unwrap(), "Belt"])
        .output()
        .expect("failed to run resq");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--force"), "got: {stderr}");
}
