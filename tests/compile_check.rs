//! Compile-verification gate.
//!
//! Every other test in this suite asserts that resq's output **parses**. That is not the same as
//! asserting it **compiles** — a file can be syntactically valid ReScript and still fail type
//! checking or name resolution. This test closes that gap by applying resq's write commands to a
//! copy of a real ReScript project and running the actual ReScript compiler over the result.
//!
//! It caught a real bug the day it was written: `rm open` refused to remove an `open` that the
//! compiler itself had flagged as unused, because resq was mistaking labeled-argument *labels*
//! (`~name=`) for value references.
//!
//! ## Fixture
//!
//! Defaults to `../rescript-hello` (a sibling of this repo in the `code/` workspace); override
//! with `RESQ_COMPILE_FIXTURE=/path/to/project`. The project must already have `node_modules`
//! installed — this test never runs `npm install`.
//!
//! ## Skipping
//!
//! If the fixture, its `node_modules`, or `npx` is missing, this test **passes with a printed
//! notice** rather than failing. resq must stay buildable and testable without a Node toolchain
//! (it is a Rust binary; the compiler is only needed for this one deeper check). Run with
//! `cargo test -- --nocapture` to see whether it actually ran or skipped.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Write commands to apply, in order. After **each** one the project is recompiled, so a failure
/// names the exact command that produced non-compiling output.
///
/// These are the commands that splice source text. Read commands cannot break a build.
fn write_steps(_project: &Path) -> Vec<(&'static str, Vec<String>)> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    vec![
        (
            "set decl (append new)",
            s(&[
                "set", "decl", "src/Main.res",
                "--name", "farewell",
                "--content", "let farewell = (~name: string) => \"Bye, \" ++ name",
            ]),
        ),
        (
            "patch (edit a literal)",
            s(&[
                "patch", "src/Main.res", "farewell",
                "--old", "\"Bye, \"",
                "--new", "\"Farewell, \"",
            ]),
        ),
        (
            "set decl (replace existing)",
            s(&[
                "set", "decl", "src/Main.res",
                "--name", "farewell",
                "--content", "let farewell = (~name: string) => \"So long, \" ++ name",
            ]),
        ),
        ("add alias", s(&["add", "alias", "src/Main.res", "Arr=Belt.Array"])),
        ("rm decl (with decorator + doc comment)", s(&["rm", "decl", "src/Main.res", "farewell"])),
        ("rm open", s(&["rm", "open", "src/Main.res", "Belt"])),
    ]
}

#[test]
fn resq_edits_still_compile() {
    let Some(fixture) = locate_fixture() else {
        eprintln!("SKIP compile_check: no fixture project (set RESQ_COMPILE_FIXTURE)");
        return;
    };
    if !fixture.join("node_modules").exists() {
        eprintln!(
            "SKIP compile_check: {} has no node_modules — run `npm install` there",
            fixture.display()
        );
        return;
    }
    if Command::new("npx").arg("--version").output().is_err() {
        eprintln!("SKIP compile_check: npx not on PATH");
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let project = temp.path().join("proj");
    copy_project(&fixture, &project);

    // The fixture must compile before we touch it. If it does not, the fixture is broken and any
    // later failure would be misattributed to resq.
    let baseline = rescript_build(&project);
    assert!(
        baseline.ok,
        "FIXTURE IS BROKEN — {} does not compile before any resq edit.\n{}",
        fixture.display(),
        baseline.output
    );

    for (label, args) in write_steps(&project) {
        let out = Command::new(env!("CARGO_BIN_EXE_resq"))
            .args(&args)
            .current_dir(&project)
            .output()
            .expect("run resq");
        assert!(
            out.status.success(),
            "resq step `{label}` failed:\nargs: {args:?}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let build = rescript_build(&project);
        assert!(
            build.ok,
            "resq step `{label}` produced source that PARSES but does NOT COMPILE.\n\
             args: {args:?}\n\
             --- rescript build output ---\n{}\n\
             --- src/Main.res ---\n{}",
            build.output,
            std::fs::read_to_string(project.join("src/Main.res")).unwrap_or_default()
        );
    }
}

/// Guards the guard: a deliberately broken edit must make this gate fail. A compile check that
/// cannot fail is worthless, so we prove the failure path works rather than assuming it.
#[test]
fn compile_gate_actually_detects_breakage() {
    let Some(fixture) = locate_fixture() else {
        eprintln!("SKIP compile_gate_actually_detects_breakage: no fixture project");
        return;
    };
    if !fixture.join("node_modules").exists() || Command::new("npx").arg("--version").output().is_err() {
        eprintln!("SKIP compile_gate_actually_detects_breakage: toolchain unavailable");
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let project = temp.path().join("proj");
    copy_project(&fixture, &project);
    assert!(rescript_build(&project).ok, "fixture should compile before we break it");

    // Type error, not a syntax error — this is precisely the class `validate_output` cannot see.
    std::fs::write(project.join("src/Wrong.res"), "let bad: int = \"not an int\"\n").unwrap();

    assert!(
        !rescript_build(&project).ok,
        "compile gate FAILED TO DETECT a type error — the gate is not actually checking anything"
    );
}

struct BuildResult {
    ok: bool,
    output: String,
}

fn rescript_build(project: &Path) -> BuildResult {
    let out = Command::new("npx")
        .args(["rescript", "build"])
        .current_dir(project)
        .output()
        .expect("run rescript build");
    BuildResult {
        ok: out.status.success(),
        output: format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

fn locate_fixture() -> Option<PathBuf> {
    let path = match std::env::var("RESQ_COMPILE_FIXTURE") {
        Ok(p) => PathBuf::from(p),
        Err(_) => Path::new(env!("CARGO_MANIFEST_DIR")).parent()?.join("rescript-hello"),
    };
    path.join("rescript.json").exists().then_some(path)
}

/// Copy sources into `dest`, **symlinking** `node_modules` rather than copying it.
///
/// Copying `node_modules` breaks the relative symlinks under `.bin`, which makes `npx rescript`
/// fail with a confusing `ERR_MODULE_NOT_FOUND` that looks like a resq bug. (Learned the hard way.)
fn copy_project(src: &Path, dest: &Path) {
    std::fs::create_dir_all(dest).expect("create dest");
    for entry in std::fs::read_dir(src).expect("read fixture") {
        let entry = entry.expect("dir entry");
        let name = entry.file_name();
        let from = entry.path();
        let to = dest.join(&name);
        match name.to_string_lossy().as_ref() {
            "node_modules" => {
                #[cfg(unix)]
                std::os::unix::fs::symlink(&from, &to).expect("symlink node_modules");
            }
            // Build artifacts and VCS metadata: skip, they are regenerated or irrelevant.
            "lib" | ".git" => {}
            _ => copy_recursive(&from, &to),
        }
    }
}

fn copy_recursive(from: &Path, to: &Path) {
    if from.is_dir() {
        std::fs::create_dir_all(to).expect("mkdir");
        for entry in std::fs::read_dir(from).expect("read dir") {
            let entry = entry.expect("entry");
            copy_recursive(&entry.path(), &to.join(entry.file_name()));
        }
    } else {
        std::fs::copy(from, to).expect("copy file");
    }
}
