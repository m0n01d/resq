//! `resq grep` — regex search over ReScript sources, annotated with the enclosing declaration's
//! dot-path (SPEC §3.1, §4).
//!
//! This is a READ command (SPEC §2): tolerant of parse errors. A file that fails to parse (or
//! parses with an ERROR node — e.g. one of the known upstream grammar gaps, SPEC §0.1) still gets
//! searched with plain regex; it just loses declaration-context annotation and comment/string
//! exclusion for that one file. A broken file must never abort the whole run — siblings still get
//! searched and reported.
//!
//! Two things make this more than a shell alias around `rg`:
//!
//! 1. **Enclosing-declaration annotation.** Every match is resolved to the *innermost* addressable
//!    declaration containing it — a top-level `let`, or one nested inside `module { … }` blocks
//!    arbitrarily deep (SPEC §3.1). Only module-structural nesting counts: a local `let` inside a
//!    function body is not itself a new addressable level (mirrors elmq, whose `refs.rs` doc notes
//!    "only top-level declarations are tracked"; here it's "only module-nested declarations").
//! 2. **Node-kind-aware exclusion.** By default, matches inside comments (`block_comment` —
//!    including `/**` doc comments, which are ordinary `block_comment` nodes per SPEC §1 finding 2
//!    — and `line_comment`) and string literals (`string`, `polyvar_string`, and the literal
//!    portions of `template_string`) are excluded. `--include-comments` / `--include-strings` opt
//!    each back in independently. Template strings get special handling: the literal text is
//!    excluded but a `${…}` interpolation is real code and is walked normally, so a match inside an
//!    interpolated expression is never wrongly suppressed.
//!
//! Byte offsets from tree-sitter are UTF-8 byte offsets, not character offsets — every line/column
//! computed here goes through [`parser::byte_offset_to_line_col`], which already gets this right,
//! rather than re-deriving it (that's the classic bug in this command, per SPEC).

use crate::cli::Format;
use crate::parser;
use crate::{DeclarationKind, ModulePath};
use anyhow::{Context, Result};
use ignore::WalkBuilder;
use regex::{Regex, RegexBuilder};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tree_sitter::Node;

/// User-facing arguments for `resq grep`, one-to-one with the clap derive in `src/cli.rs`.
#[derive(Clone)]
pub struct GrepArgs {
    pub pattern: String,
    pub path: Option<PathBuf>,
    pub fixed: bool,
    pub ignore_case: bool,
    pub include_comments: bool,
    pub include_strings: bool,
    pub definitions: bool,
    pub source: bool,
    pub format: Format,
}

/// One regex match, annotated with its enclosing declaration (if any).
pub struct Hit {
    pub file: PathBuf,
    /// 1-indexed line of the match.
    pub line: usize,
    /// 1-indexed column of the match, counting characters not bytes.
    pub column: usize,
    /// The enclosing declaration's canonical dot-path, per [`crate::Declaration::primary_path`].
    /// `None` when the match falls outside every addressable declaration (e.g. a stray `open` line
    /// or a file with parse errors).
    pub path: Option<ModulePath>,
    pub kind: Option<DeclarationKind>,
    pub match_text: String,
    pub line_text: String,
    /// The enclosing declaration's full source, including decorators/doc comment — populated only
    /// when there is an enclosing declaration. Used by `--source`.
    pub decl_source: Option<String>,
    pub decl_start_line: Option<usize>,
    pub decl_end_line: Option<usize>,
}

/// Top-level entry point called from `main.rs`. Returns the process exit code: `0` = matches
/// found, `1` = no matches, `2` = error. Mirrors `grep`/`rg`.
pub fn execute(args: GrepArgs) -> i32 {
    match run(&args) {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(e) => {
            eprintln!("error: {e:#}");
            2
        }
    }
}

fn run(args: &GrepArgs) -> Result<bool> {
    let hits = search(args)?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let any = if args.source {
        emit_source(&hits, args, &mut out)?
    } else {
        emit_hits(&hits, args, &mut out)?
    };

    out.flush().ok();
    Ok(any)
}

/// Collect every match, already filtered by `--include-comments` / `--include-strings` /
/// `--definitions` and annotated with its enclosing declaration. Pure and side-effect-free (no
/// printing), so it's the seam tests exercise directly.
pub fn search(args: &GrepArgs) -> Result<Vec<Hit>> {
    let regex = build_regex(args)?;
    let files = discover_files(args.path.as_deref())?;
    let mut hits = Vec::new();

    for file in &files {
        let source = match fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("warning: could not read {}: {}", file.display(), e);
                continue;
            }
        };

        let (decl_ranges, excluded_ranges) = match parser::parse(&source) {
            Ok(tree) if !tree.root_node().has_error() => {
                let mut decls = Vec::new();
                collect_decl_ranges(tree.root_node(), &source, &ModulePath::root(), &mut decls);
                let excluded = collect_excluded_ranges(tree.root_node(), &source);
                (decls, excluded)
            }
            Ok(_) => {
                eprintln!(
                    "warning: {} has parse errors; reporting matches without declaration context",
                    file.display()
                );
                (Vec::new(), Vec::new())
            }
            Err(e) => {
                eprintln!(
                    "warning: could not parse {} as ReScript ({e:#}); reporting matches without declaration context",
                    file.display()
                );
                (Vec::new(), Vec::new())
            }
        };

        for mat in regex.find_iter(&source) {
            let offset = mat.start();

            if let Some(kind) = offset_excluded_kind(offset, &excluded_ranges) {
                let allowed = match kind {
                    ExcludedKind::Comment => args.include_comments,
                    ExcludedKind::String => args.include_strings,
                };
                if !allowed {
                    continue;
                }
            }

            let enclosing = enclosing_decl(offset, &decl_ranges);

            if args.definitions {
                let in_name = enclosing
                    .is_some_and(|d| d.name_spans.iter().any(|&(s, e)| offset >= s && offset < e));
                if !in_name {
                    continue;
                }
            }

            let (line, column) = parser::byte_offset_to_line_col(&source, offset);
            let line_text = source.lines().nth(line - 1).unwrap_or("").to_string();

            hits.push(Hit {
                file: file.clone(),
                line,
                column,
                path: enclosing.map(|d| d.primary_path.clone()),
                kind: enclosing.map(|d| d.kind),
                match_text: mat.as_str().to_string(),
                line_text,
                decl_source: enclosing.map(|d| source[d.full_start..d.full_end].to_string()),
                decl_start_line: enclosing
                    .map(|d| parser::byte_offset_to_line_col(&source, d.full_start).0),
                decl_end_line: enclosing.map(|d| {
                    parser::byte_offset_to_line_col(&source, d.full_end.saturating_sub(1)).0
                }),
            });
        }
    }

    Ok(hits)
}

fn emit_hits(hits: &[Hit], args: &GrepArgs, out: &mut impl Write) -> Result<bool> {
    let mut any = false;
    for hit in hits {
        any = true;
        match emit(out, hit, args) {
            Ok(()) => {}
            Err(e) if is_broken_pipe(&e) => return Ok(any),
            Err(e) => return Err(e),
        }
    }
    Ok(any)
}

fn emit(out: &mut impl Write, hit: &Hit, args: &GrepArgs) -> Result<()> {
    match args.format {
        Format::Compact => {
            let decl = hit
                .path
                .as_ref()
                .map(|p| p.to_string())
                .unwrap_or_else(|| "-".to_string());
            writeln!(
                out,
                "{}:{}:{}:{}",
                hit.file.display(),
                hit.line,
                decl,
                hit.line_text
            )?;
        }
        Format::Json => {
            let value = serde_json::json!({
                "file": hit.file.display().to_string(),
                "line": hit.line,
                "column": hit.column,
                "decl": hit.path.as_ref().map(|p| p.to_string()),
                "decl_kind": hit.kind.map(|k| k.to_string()),
                "match": hit.match_text,
                "line_text": hit.line_text,
            });
            writeln!(out, "{}", serde_json::to_string(&value)?)?;
        }
    }
    Ok(())
}

/// One buffered `--source` block: the full text of a declaration, printed once even if it has
/// several matches, with a running count.
struct SourceBlock {
    file: PathBuf,
    path: ModulePath,
    kind: DeclarationKind,
    source: String,
    start_line: usize,
    end_line: usize,
    count: usize,
}

/// `--source`: print the enclosing declaration's full source rather than the matching line.
/// Matches outside any declaration are skipped — there is no declaration source to print for them.
fn emit_source(hits: &[Hit], args: &GrepArgs, out: &mut impl Write) -> Result<bool> {
    let mut blocks: Vec<SourceBlock> = Vec::new();
    for hit in hits {
        let (Some(path), Some(kind), Some(source), Some(start_line), Some(end_line)) = (
            hit.path.as_ref(),
            hit.kind,
            hit.decl_source.as_ref(),
            hit.decl_start_line,
            hit.decl_end_line,
        ) else {
            continue;
        };

        if let Some(block) = blocks
            .iter_mut()
            .find(|b| b.file == hit.file && &b.path == path)
        {
            block.count += 1;
        } else {
            blocks.push(SourceBlock {
                file: hit.file.clone(),
                path: path.clone(),
                kind,
                source: source.clone(),
                start_line,
                end_line,
                count: 1,
            });
        }
    }

    let any = !blocks.is_empty();
    match args.format {
        Format::Compact => {
            let bare = blocks.len() == 1;
            for (i, block) in blocks.iter().enumerate() {
                if i > 0 {
                    writeln!(out)?;
                }
                if !bare {
                    writeln!(out, "## {}:{}", block.file.display(), block.path)?;
                }
                write!(out, "{}", block.source)?;
                if !block.source.ends_with('\n') {
                    writeln!(out)?;
                }
            }
        }
        Format::Json => {
            for block in &blocks {
                let value = serde_json::json!({
                    "file": block.file.display().to_string(),
                    "path": block.path.to_string(),
                    "kind": block.kind.to_string(),
                    "source": block.source,
                    "start_line": block.start_line,
                    "end_line": block.end_line,
                    "match_count": block.count,
                });
                writeln!(out, "{}", serde_json::to_string(&value)?)?;
            }
        }
    }
    Ok(any)
}

fn is_broken_pipe(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe)
    })
}

// ---------------------------------------------------------------------------------------------
// regex
// ---------------------------------------------------------------------------------------------

fn build_regex(args: &GrepArgs) -> Result<Regex> {
    let pat = if args.fixed {
        regex::escape(&args.pattern)
    } else {
        args.pattern.clone()
    };
    RegexBuilder::new(&pat)
        .case_insensitive(args.ignore_case)
        .build()
        .with_context(|| format!("invalid regex: {}", args.pattern))
}

// ---------------------------------------------------------------------------------------------
// file discovery — deliberately independent of A4's `project.rs` (SPEC dispatch note: A5 must not
// depend on it). Walks the given path (defaulting to the current directory), honoring
// `.gitignore` and skipping `node_modules`/`lib` directories, collecting `.res`/`.resi` files.
// ---------------------------------------------------------------------------------------------

fn discover_files(path: Option<&Path>) -> Result<Vec<PathBuf>> {
    let root = match path {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir().context("could not determine current directory")?,
    };

    if !root.exists() {
        anyhow::bail!("path not found: {}", root.display());
    }
    if root.is_file() {
        return Ok(vec![root]);
    }

    let mut files = Vec::new();
    let walker = WalkBuilder::new(&root)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .ignore(true)
        .filter_entry(|entry| {
            !(entry.file_type().is_some_and(|ft| ft.is_dir())
                && matches!(entry.file_name().to_str(), Some("node_modules") | Some("lib")))
        })
        .build();

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                eprintln!("warning: {e}");
                continue;
            }
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.into_path();
        if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("res") | Some("resi")
        ) {
            files.push(path);
        }
    }

    files.sort();
    Ok(files)
}

// ---------------------------------------------------------------------------------------------
// declaration ranges — only module-structural nesting produces a new addressable level (SPEC
// §3.1): the root file scope, and the `block` body of a `module … = { … }`/functor. A `let`
// nested inside a function body is not itself walked for further declarations, so a match deep
// inside an expression resolves to the nearest *addressable* declaration, exactly like elmq's
// "only top-level declarations are tracked" rule, generalized to ReScript's nested modules.
// ---------------------------------------------------------------------------------------------

struct DeclRange {
    /// The canonical dot-path this range answers to (`Declaration::primary_path`).
    primary_path: ModulePath,
    kind: DeclarationKind,
    /// Byte span including decorators/doc comment (`parser::decl_span_with_attachments`).
    full_start: usize,
    full_end: usize,
    /// Byte spans of the declaration's own name(s), for `--definitions`.
    name_spans: Vec<(usize, usize)>,
}

fn collect_decl_ranges(node: Node, src: &str, path: &ModulePath, out: &mut Vec<DeclRange>) {
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();

    for child in children {
        if !parser::DECLARATION_KINDS.contains(&child.kind()) {
            continue;
        }
        let Some(decl) = parser::declaration_from_node(child, src, path) else {
            continue;
        };
        let (full_start, _) = parser::decl_span_with_attachments(child, src);
        let full_end = child.end_byte();
        let name_spans = collect_name_spans(child, src);
        let primary_path = decl.primary_path();
        let kind = decl.kind;

        out.push(DeclRange {
            primary_path,
            kind,
            full_start,
            full_end,
            name_spans,
        });

        if kind == DeclarationKind::Module {
            let mut bcursor = child.walk();
            let bindings: Vec<Node> = child
                .named_children(&mut bcursor)
                .filter(|c| c.kind() == "module_binding")
                .collect();
            for binding in bindings {
                let Some(name_node) = binding.child_by_field_name("name") else {
                    continue;
                };
                let Ok(name) = name_node.utf8_text(src.as_bytes()) else {
                    continue;
                };
                let child_path = path.child(name);
                if let Some(body) = module_binding_body(binding) {
                    collect_decl_ranges(body, src, &child_path, out);
                }
            }
        }
    }
}

/// The block a `module_binding` defines, whatever shape it takes (SPEC §1 finding 9 /
/// `parser::module_body_block`, reproduced per-binding here since one `module_declaration` may
/// hold several `module_binding`s, each with its own body).
fn module_binding_body(binding: Node) -> Option<Node> {
    let definition = binding
        .child_by_field_name("definition")
        .or_else(|| binding.child_by_field_name("signature"))?;
    match definition.kind() {
        "block" => Some(definition),
        "functor" => definition.child_by_field_name("body"),
        _ => None,
    }
}

fn enclosing_decl(offset: usize, decls: &[DeclRange]) -> Option<&DeclRange> {
    // Ranges nest (a module's range fully contains its members'), never partially overlap, so the
    // smallest containing range is always the innermost — the correct enclosing declaration.
    decls
        .iter()
        .filter(|d| d.full_start <= offset && offset < d.full_end)
        .min_by_key(|d| d.full_end - d.full_start)
}

/// Byte spans of the name(s) a declaration node binds, for `--definitions`. Dispatches by kind the
/// same way `parser::declaration_from_node` does. Pattern binders come from
/// `parser::bound_name_spans`, the single implementation of the `record_pattern`
/// field-vs-binder disambiguation (SPEC §1 finding 8) — do not reimplement it here.
fn collect_name_spans(node: Node, src: &str) -> Vec<(usize, usize)> {
    match node.kind() {
        "let_declaration" => {
            let mut spans = Vec::new();
            let mut cursor = node.walk();
            let bindings: Vec<Node> = node
                .named_children(&mut cursor)
                .filter(|c| c.kind() == "let_binding")
                .collect();
            for binding in bindings {
                for i in 0..binding.child_count() as u32 {
                    if binding.field_name_for_child(i) != Some("pattern") {
                        continue;
                    }
                    if let Some(pattern) = binding.child(i) {
                        spans.extend(parser::bound_name_spans(pattern, src));
                    }
                }
            }
            spans
        }
        "type_declaration" => {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .filter(|c| c.kind() == "type_binding")
                .filter_map(|b| b.child_by_field_name("name"))
                .map(|n| (n.start_byte(), n.end_byte()))
                .collect()
        }
        "module_declaration" => {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .filter(|c| c.kind() == "module_binding")
                .filter_map(|b| b.child_by_field_name("name"))
                .map(|n| (n.start_byte(), n.end_byte()))
                .collect()
        }
        "external_declaration" => {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .filter(|c| c.kind() == "value_identifier")
                .map(|n| (n.start_byte(), n.end_byte()))
                .collect()
        }
        "include_statement" | "open_statement" => node
            .named_child(0)
            .map(|n| vec![(n.start_byte(), n.end_byte())])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------------------------
// excluded ranges — comments and string literals (SPEC §1: `block_comment`, `line_comment`,
// `string`, `template_string`). A `/** doc */` comment is an ordinary `block_comment` (no distinct
// doc-comment node kind, per `parser::is_doc_comment`'s doc), so it is excluded like any other
// comment by default and included by `--include-comments` like any other.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExcludedKind {
    Comment,
    String,
}

#[derive(Clone, Copy)]
struct ExcludedRange {
    start: usize,
    end: usize,
    kind: ExcludedKind,
}

fn collect_excluded_ranges(root: Node, src: &str) -> Vec<ExcludedRange> {
    let mut out = Vec::new();
    walk_excluded(root, src, &mut out);
    out.sort_by_key(|r| r.start);
    out
}

fn walk_excluded(node: Node, src: &str, out: &mut Vec<ExcludedRange>) {
    match node.kind() {
        "block_comment" | "line_comment" => {
            out.push(ExcludedRange {
                start: node.start_byte(),
                end: node.end_byte(),
                kind: ExcludedKind::Comment,
            });
            return;
        }
        "string" | "polyvar_string" => {
            out.push(ExcludedRange {
                start: node.start_byte(),
                end: node.end_byte(),
                kind: ExcludedKind::String,
            });
            return;
        }
        "template_string_content" => {
            collect_template_content(node, src, out);
            return;
        }
        _ => {}
    }
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    for child in children {
        walk_excluded(child, src, out);
    }
}

/// `template_string_content`'s only *named* children are `escape_sequence` and
/// `template_substitution` (SPEC §1 node shapes); the literal text between them is unnamed and has
/// no node of its own. Walk the named children in order, treating the gaps and `escape_sequence`
/// as literal String content, but recursing normally into `template_substitution` — that's real
/// interpolated code (`${expr}`), not string content, and must not be suppressed as if it were.
fn collect_template_content(content: Node, src: &str, out: &mut Vec<ExcludedRange>) {
    let mut pos = content.start_byte();
    let mut cursor = content.walk();
    let children: Vec<Node> = content.named_children(&mut cursor).collect();

    for child in children {
        if child.start_byte() > pos {
            out.push(ExcludedRange {
                start: pos,
                end: child.start_byte(),
                kind: ExcludedKind::String,
            });
        }
        if child.kind() == "template_substitution" {
            walk_excluded(child, src, out);
        } else {
            out.push(ExcludedRange {
                start: child.start_byte(),
                end: child.end_byte(),
                kind: ExcludedKind::String,
            });
        }
        pos = child.end_byte();
    }

    if pos < content.end_byte() {
        out.push(ExcludedRange {
            start: pos,
            end: content.end_byte(),
            kind: ExcludedKind::String,
        });
    }
}

fn offset_excluded_kind(offset: usize, ranges: &[ExcludedRange]) -> Option<ExcludedKind> {
    for r in ranges {
        if r.start > offset {
            break;
        }
        if offset < r.end {
            return Some(r.kind);
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------
// internal unit tests — white-box checks on helpers that aren't part of the public surface.
// End-to-end, fixture-driven behavior lives in `tests/grep.rs`.
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_args(pattern: &str) -> GrepArgs {
        GrepArgs {
            pattern: pattern.to_string(),
            path: None,
            fixed: false,
            ignore_case: false,
            include_comments: false,
            include_strings: false,
            definitions: false,
            source: false,
            format: Format::Compact,
        }
    }

    #[test]
    fn build_regex_fixed_mode_escapes_metachars() {
        let mut args = sample_args("a.b");
        args.fixed = true;
        let re = build_regex(&args).unwrap();
        assert!(re.is_match("a.b"));
        assert!(!re.is_match("aXb"));
    }

    #[test]
    fn build_regex_case_insensitive() {
        let mut args = sample_args("http");
        args.ignore_case = true;
        let re = build_regex(&args).unwrap();
        assert!(re.is_match("Http"));
        assert!(re.is_match("HTTP"));
    }

    #[test]
    fn invalid_regex_returns_error() {
        let args = sample_args("[unclosed");
        assert!(build_regex(&args).is_err());
    }

    #[test]
    fn comment_and_string_ranges_are_classified() {
        let src = "/** doc */\nlet x = \"hello\"\n// line\nlet y = 1\n";
        let tree = parser::parse(src).unwrap();
        let ranges = collect_excluded_ranges(tree.root_node(), src);

        let doc_offset = src.find("doc").unwrap();
        assert_eq!(
            offset_excluded_kind(doc_offset, &ranges),
            Some(ExcludedKind::Comment)
        );

        let hello_offset = src.find("hello").unwrap();
        assert_eq!(
            offset_excluded_kind(hello_offset, &ranges),
            Some(ExcludedKind::String)
        );

        let line_offset = src.find("line").unwrap();
        assert_eq!(
            offset_excluded_kind(line_offset, &ranges),
            Some(ExcludedKind::Comment)
        );

        let y_offset = src.find("let y").unwrap();
        assert_eq!(offset_excluded_kind(y_offset, &ranges), None);
    }

    #[test]
    fn template_string_interpolation_is_not_excluded() {
        // The literal text is string content; the `${name}` expression is real code and must
        // remain searchable (and, if it contained a string/comment of its own, still excluded).
        let src = "let greeting = `hello ${name}!`\n";
        let tree = parser::parse(src).unwrap();
        assert!(!tree.root_node().has_error(), "fixture must parse cleanly");
        let ranges = collect_excluded_ranges(tree.root_node(), src);

        let hello_offset = src.find("hello").unwrap();
        assert_eq!(
            offset_excluded_kind(hello_offset, &ranges),
            Some(ExcludedKind::String)
        );

        let name_offset = src.find("name").unwrap();
        assert_eq!(
            offset_excluded_kind(name_offset, &ranges),
            None,
            "the interpolated expression is code, not string content"
        );
    }

    #[test]
    fn nested_module_declarations_are_reachable() {
        let src = "module Inner = {\n  module Deep = {\n    let deepValue = 42\n  }\n}\n";
        let tree = parser::parse(src).unwrap();
        let mut decls = Vec::new();
        collect_decl_ranges(tree.root_node(), src, &ModulePath::root(), &mut decls);

        let deep_offset = src.find("42").unwrap();
        let decl = enclosing_decl(deep_offset, &decls).expect("enclosing decl");
        assert_eq!(decl.primary_path.to_string(), "Inner.Deep.deepValue");
        assert_eq!(decl.kind, DeclarationKind::Let);
    }
}
