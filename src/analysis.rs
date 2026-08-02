//! `resq list` — walk a parsed tree into a [`FileSummary`] and render it.
//!
//! WAVE 2 (agent A2) owns this file. Ported from elmq's `extract_summary` (see
//! <https://raw.githubusercontent.com/caseyWebb/elmq/main/src/parser.rs>), adapted for ReScript's
//! nested-module addressing (SPEC §3.1) instead of Elm's flat, single-module files.
//!
//! `list` is a **read** command (SPEC §2): it must tolerate ERROR nodes, warn on stderr, and still
//! print a summary of the well-formed portions, exiting 0. It therefore calls [`parser::parse`]
//! directly and never [`parser::ensure_clean_parse`].

use crate::cli::Format;
use crate::parser;
use crate::{BinderKind, Declaration, DeclarationKind, FileSummary, ModuleAlias, ModulePath};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use tree_sitter::{Node, Tree};

// -------------------------------------------------------------------------------------------
// CLI entry point
// -------------------------------------------------------------------------------------------

/// Handler for `Command::List`. Prints one summary per file to stdout; on a file with parse
/// errors, warns on stderr and still prints the summary for the well-formed portions (SPEC §2).
///
/// Only genuine I/O failures (e.g. a missing file) propagate as an error — a bad parse never
/// does.
pub fn run_list(files: Vec<PathBuf>, format: Format, docs: bool) -> Result<()> {
    for (i, file) in files.iter().enumerate() {
        if i > 0 {
            println!();
        }
        list_one(file, &format, docs)?;
    }
    Ok(())
}

fn list_one(file: &Path, format: &Format, docs: bool) -> Result<()> {
    let source = std::fs::read_to_string(file)
        .with_context(|| format!("failed to read {}", file.display()))?;
    let tree = parser::parse(&source)?;

    if tree.root_node().has_error() {
        match parser::first_error_location(&tree, &source) {
            Some((line, col)) => eprintln!(
                "warning: {}: parse errors starting at {line}:{col}; showing partial summary",
                file.display()
            ),
            None => eprintln!(
                "warning: {}: parse errors; showing partial summary",
                file.display()
            ),
        }
    }

    let module_name = crate::module_name_from_path(file);
    let summary = extract_summary(&tree, &source, module_name);

    match format {
        Format::Compact => {
            let total_lines = source.lines().count().max(1);
            print!("{}", render_compact(&summary, total_lines, docs));
        }
        Format::Json => {
            let value = to_json_summary(&summary);
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
    }

    Ok(())
}

// -------------------------------------------------------------------------------------------
// Extraction
// -------------------------------------------------------------------------------------------

/// Walk a parsed tree into a [`FileSummary`].
///
/// The result is **flat and source-ordered**; nesting is carried entirely by
/// [`Declaration::path`] (SPEC — `FileSummary.declarations` doc comment). A module's own
/// `Declaration` is pushed before its members, matching source order.
///
/// Tolerant by construction: nodes that are not one of [`parser::DECLARATION_KINDS`] (including
/// ERROR/MISSING nodes produced by a broken parse) simply don't match
/// [`parser::declaration_kind`] and are skipped, so a locally-broken file still yields a summary
/// of everything that parsed cleanly.
pub fn extract_summary(tree: &Tree, src: &str, module_name: impl Into<String>) -> FileSummary {
    let mut summary = FileSummary::new(module_name);
    walk_block(tree.root_node(), src, &ModulePath::root(), &mut summary);
    summary
}

/// Walk the direct named children of a `source_file` or module `block`, recursing into nested
/// module bodies. `path` is the module path enclosing `node`'s children.
///
/// `open` statements and `module A = B.C` aliases feed `summary.opens` / `summary.aliases`
/// exclusively — they are not pushed onto `summary.declarations`. Neither has members to recurse
/// into, and neither is addressed by dot-path anywhere in the command surface (`add/rm open` and
/// `add alias` take literal module names, SPEC §3.2/§4) — matching the compact-render contract,
/// where an alias appears under `aliases:` and never a second time under `modules:`.
fn walk_block(node: Node, src: &str, path: &ModulePath, summary: &mut FileSummary) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let Some(kind) = parser::declaration_kind(child) else {
            continue;
        };

        if kind == DeclarationKind::Open {
            if let Some(target) = open_target_text(child, src) {
                summary.opens.push(target);
            }
            continue;
        }

        if kind == DeclarationKind::Module
            && let Some((name, target)) = parser::module_alias_parts(child, src)
        {
            summary.aliases.push(ModuleAlias { name, target });
            continue;
        }

        let decl = parser::declaration_from_node(child, src, path);

        // Needed before `decl` is moved into `summary.declarations`, to descend into a module
        // body under the right child path.
        let primary_name = decl.as_ref().and_then(|d| d.names.first().cloned());

        if let Some(d) = decl {
            summary.declarations.push(d);
        }

        if kind == DeclarationKind::Module
            && let Some(body) = parser::module_body_block(child)
            && let Some(name) = primary_name
        {
            walk_block(body, src, &path.child(name), summary);
        }
    }
}

/// `open Belt.Array`'s single named child, as text — SPEC §1: `open_statement child:
/// module_identifier` (`module_expression` is a supertype, so this also covers dotted paths).
fn open_target_text(node: Node, src: &str) -> Option<String> {
    node.named_child(0)
        .and_then(|c| c.utf8_text(src.as_bytes()).ok())
        .map(str::to_string)
}

// -------------------------------------------------------------------------------------------
// Compact rendering
// -------------------------------------------------------------------------------------------

/// Render a [`FileSummary`] in the default `--format compact` style, modelled on elmq's output:
/// grouped by kind at the file root (`opens`, `aliases`, `types`, `functions`, `externals`,
/// `includes`, `modules`); a nested module's own members are rendered as an indented tree in
/// source order, depth taken from `path.len()`, rather than re-grouped by kind — grouping only
/// makes sense at the top of the file, where an agent is scanning for "what's in here".
///
/// `--docs` additionally prints each declaration's doc comment, indented beneath it.
pub fn render_compact(summary: &FileSummary, total_lines: usize, docs: bool) -> String {
    let by_path = group_by_path(&summary.declarations);
    let root: Vec<&Declaration> = by_path
        .get(&ModulePath::root())
        .cloned()
        .unwrap_or_default();

    let mut out = String::new();
    let _ = writeln!(out, "module {}  ({total_lines} lines)", summary.module_name);

    let mut blocks: Vec<String> = Vec::new();

    // `opens` and `aliases` are visually one "imports" block — no blank line between them.
    let mut imports = String::new();
    if !summary.opens.is_empty() {
        let _ = writeln!(imports, "opens:");
        for m in &summary.opens {
            let _ = writeln!(imports, "  {m}");
        }
    }
    if !summary.aliases.is_empty() {
        let _ = writeln!(imports, "aliases:");
        for a in &summary.aliases {
            let _ = writeln!(imports, "  {} = {}", a.name, a.target);
        }
    }
    if !imports.is_empty() {
        blocks.push(imports.trim_end_matches('\n').to_string());
    }

    const KIND_SECTIONS: [(&str, DeclarationKind); 4] = [
        ("types", DeclarationKind::Type),
        ("functions", DeclarationKind::Let),
        ("externals", DeclarationKind::External),
        ("includes", DeclarationKind::Include),
    ];
    for (title, kind) in KIND_SECTIONS {
        let decls: Vec<&Declaration> = root.iter().filter(|d| d.kind == kind).copied().collect();
        if decls.is_empty() {
            continue;
        }
        let mut block = String::new();
        let _ = writeln!(block, "{title}:");
        render_members(&decls, &by_path, 1, docs, &mut block);
        blocks.push(block.trim_end_matches('\n').to_string());
    }

    let modules: Vec<&Declaration> = root
        .iter()
        .filter(|d| d.kind == DeclarationKind::Module)
        .copied()
        .collect();
    if !modules.is_empty() {
        let mut block = String::new();
        let _ = writeln!(block, "modules:");
        render_members(&modules, &by_path, 1, docs, &mut block);
        blocks.push(block.trim_end_matches('\n').to_string());
    }

    for block in blocks {
        out.push('\n');
        out.push_str(&block);
        out.push('\n');
    }

    out
}

fn group_by_path(decls: &[Declaration]) -> HashMap<ModulePath, Vec<&Declaration>> {
    let mut map: HashMap<ModulePath, Vec<&Declaration>> = HashMap::new();
    for d in decls {
        map.entry(d.path.clone()).or_default().push(d);
    }
    map
}

/// Render one column-aligned list of sibling declarations at `indent` levels (2 spaces each),
/// recursing into a module's own members (looked up by its full path in `by_path`) immediately
/// beneath it rather than deferring recursion to a later pass — this is what keeps `Inner`'s
/// members interleaved in source order instead of re-grouped by kind.
fn render_members(
    decls: &[&Declaration],
    by_path: &HashMap<ModulePath, Vec<&Declaration>>,
    indent: usize,
    docs: bool,
    out: &mut String,
) {
    let width = decls
        .iter()
        .map(|d| label(d).chars().count())
        .max()
        .unwrap_or(0);
    let pad = "  ".repeat(indent);
    for d in decls {
        let l = label(d);
        let range = line_range(d);
        let _ = writeln!(out, "{pad}{l:width$}  {range}");

        if docs && let Some(doc) = &d.doc_comment {
            let doc_pad = "  ".repeat(indent + 1);
            for line in doc.lines() {
                let _ = writeln!(out, "{doc_pad}{line}");
            }
        }

        if d.kind == DeclarationKind::Module
            && let Some(name) = d.names.first()
        {
            let child_path = d.path.child(name.clone());
            if let Some(members) = by_path.get(&child_path) {
                render_members(members, by_path, indent + 1, docs, out);
            }
        }
    }
}

fn label(d: &Declaration) -> String {
    d.names.join(", ")
}

fn line_range(d: &Declaration) -> String {
    if d.start_line == d.end_line {
        format!("L{}", d.start_line)
    } else {
        format!("L{}-{}", d.start_line, d.end_line)
    }
}

// -------------------------------------------------------------------------------------------
// JSON rendering
// -------------------------------------------------------------------------------------------
//
// `FileSummary`'s own `Serialize` impl (src/lib.rs) serializes `Declaration.path` as-is — the
// *enclosing* module path, deliberately not the full address (see the CRITICAL SHARED-STATE
// NOTE in SPEC-adjacent docs: `path` cannot include the declaration's own name because a
// destructuring binding has several names and therefore several addresses). `list --format json`
// promises "full dot-paths", so this module serializes its own view that joins `path` with each
// bound name via `Declaration::full_paths` — never by hand-concatenating strings.

#[derive(serde::Serialize)]
struct JsonSummary {
    module_name: String,
    opens: Vec<String>,
    aliases: Vec<JsonAlias>,
    declarations: Vec<JsonDeclaration>,
}

#[derive(serde::Serialize)]
struct JsonAlias {
    name: String,
    target: String,
}

#[derive(serde::Serialize)]
struct JsonDeclaration {
    /// Every dot-path this declaration answers to, e.g. `["Inner.Deep.deepValue"]`, or several
    /// entries for a destructuring binding. Built from [`Declaration::full_paths`], not hand-rolled.
    paths: Vec<String>,
    kind: DeclarationKind,
    binder_kind: BinderKind,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    decorators: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    type_annotation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    doc_comment: Option<String>,
    start_line: usize,
    end_line: usize,
}

/// Build the JSON-serializable view of a [`FileSummary`], with full dot-paths for every
/// declaration (including nested ones).
pub fn to_json_summary(summary: &FileSummary) -> serde_json::Value {
    let js = JsonSummary {
        module_name: summary.module_name.clone(),
        opens: summary.opens.clone(),
        aliases: summary
            .aliases
            .iter()
            .map(|a| JsonAlias {
                name: a.name.clone(),
                target: a.target.clone(),
            })
            .collect(),
        declarations: summary
            .declarations
            .iter()
            .map(|d| JsonDeclaration {
                paths: d.full_paths().iter().map(ToString::to_string).collect(),
                kind: d.kind,
                binder_kind: d.binder_kind,
                decorators: d.decorators.clone(),
                type_annotation: d.type_annotation.clone(),
                doc_comment: d.doc_comment.clone(),
                start_line: d.start_line,
                end_line: d.end_line,
            })
            .collect(),
    };
    serde_json::to_value(js).expect("JsonSummary is always representable as JSON")
}
