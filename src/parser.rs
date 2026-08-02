//! Parsing spine: grammar loading, error location, attachment spans, and the single canonical
//! reader for one declaration node.
//!
//! **All node-kind knowledge lives here.** Downstream modules should call these helpers rather
//! than matching on `node.kind()` themselves — a wrong node kind fails *silently* by matching
//! nothing, which is the most expensive bug class in this port. The node shapes below were
//! verified against the pinned grammar (SPEC §1); the most important traps are:
//!
//! * The top-level wrapper is `let_declaration`, **not** `let_binding` (which is its child).
//! * Decorators and doc comments are **siblings** of the declaration, not children.
//! * `/**` doc comments are `block_comment` nodes — there is no distinct doc-comment kind.
//! * Under `type_binding`, variants sit in the `body:` field but records are an *unnamed* child.
//! * `let_binding` may have several `pattern:` children (`let (a, b) as whole = …`), and
//!   `let_declaration` may have several `let_binding` children (`let a = 1 and b = 2`).

use crate::{BinderKind, Declaration, DeclarationKind, ModulePath};
use anyhow::{Context, Result, bail};
use std::path::Path;
use tree_sitter::{Node, Parser, Tree};

/// Node kinds that resq treats as addressable declarations.
pub const DECLARATION_KINDS: &[&str] = &[
    "let_declaration",
    "type_declaration",
    "module_declaration",
    "external_declaration",
    "include_statement",
    "open_statement",
];

pub fn parse(source: &str) -> Result<Tree> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rescript::LANGUAGE.into())
        .context("failed to load the ReScript grammar")?;
    parser
        .parse(source, None)
        .context("failed to parse ReScript source")
}

/// Locate the first ERROR or MISSING node and return its 1-indexed `(line, col)`.
/// `None` when the tree is clean.
///
/// Returns the **smallest** (deepest) error node, not the outermost one. On a badly broken file
/// tree-sitter often makes the whole file a single ERROR, but on a locally broken one the useful
/// coordinate is the innermost MISSING token — reporting the outer node would point at line 1 for
/// a fault twenty lines down.
pub fn first_error_location(tree: &Tree, source: &str) -> Option<(usize, usize)> {
    let node = first_error_node(tree.root_node())?;
    Some(byte_offset_to_line_col(source, node.start_byte()))
}

fn first_error_node(node: Node<'_>) -> Option<Node<'_>> {
    if !node.has_error() && !node.is_error() && !node.is_missing() {
        return None;
    }
    // Descend first so that the deepest — most specific — error wins, and scan children in source
    // order so the earliest fault is reported. MISSING nodes are unnamed, so walk *all* children.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = first_error_node(child) {
            return Some(found);
        }
    }
    if node.is_error() || node.is_missing() {
        return Some(node);
    }
    None
}

/// 1-indexed `(line, column)` for a byte offset. Columns count characters, not bytes, so unicode
/// source lines report a coordinate a human can act on.
pub fn byte_offset_to_line_col(source: &str, offset: usize) -> (usize, usize) {
    let clamped = offset.min(source.len());
    let mut line = 1;
    let mut col = 1;
    for (i, ch) in source.char_indices() {
        if i >= clamped {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Step 1 of the write-safety invariant (SPEC §2): parse the input and refuse to proceed if it is
/// already broken. Every mutating command MUST call this before touching a buffer, so we never
/// splice an edit into a damaged CST.
///
/// This is also what makes the three known upstream grammar gaps (SPEC §0.1) safe: resq declines
/// to edit files it cannot parse rather than corrupting them.
pub fn ensure_clean_parse(source: &str, file: &Path) -> Result<Tree> {
    let tree = parse(source)?;
    if tree.root_node().has_error() {
        let where_ = match first_error_location(&tree, source) {
            Some((line, col)) => format!(" at {line}:{col}"),
            None => String::new(),
        };
        bail!(
            "refusing to edit {}: file has pre-existing parse errors{where_}",
            file.display()
        );
    }
    Ok(tree)
}

/// SPEC §1 finding 2: **there is no doc-comment node kind.** `/** doc */` and `/* block */` both
/// parse as `block_comment`; the only difference is the text prefix.
///
/// This is the one place that check is implemented. Do not re-derive it elsewhere.
pub fn is_doc_comment(node: Node, src: &str) -> bool {
    node.kind() == "block_comment"
        && node
            .utf8_text(src.as_bytes())
            .is_ok_and(|t| t.starts_with("/**"))
}

/// True for nodes that attach to the declaration that follows them.
fn is_attachment(node: Node, src: &str) -> bool {
    node.kind() == "decorator" || is_doc_comment(node, src)
}

/// The start of a declaration **including its decorators and doc comment**, as
/// `(start_byte, 1-indexed start_line)`.
///
/// SPEC §1 finding 1: decorators and doc comments are *siblings* of the declaration node, not
/// children, so `node.start_byte()` alone excludes them. Every command that reads or moves a
/// declaration must use this span — a `get` that drops `@react.component` returns code that does
/// not compile, and an `rm` that misses a decorator orphans it onto the next declaration and
/// silently changes that declaration's meaning.
///
/// Walks backwards over contiguous preceding siblings, with two stop conditions:
///
/// * a sibling that is neither a `decorator` nor a `/**` doc comment (an ordinary `/* … */`
///   comment, a `// …` comment, the `{` opening a module block, or the previous declaration);
/// * a **blank line** before a doc comment. A comment separated from the declaration by a blank
///   line is free-standing prose, not documentation, and must not be swept up.
///
/// Decorators are deliberately exempt from the blank-line rule: `@genType\n\nlet x = 1` is legal
/// ReScript and the decorator still binds to `x`, so leaving it behind on `rm` would corrupt the
/// file. See the note in the report — this is a refinement of "stop at the first blank-line gap",
/// chosen because the two attachment kinds have different consequences when dropped.
pub fn decl_span_with_attachments(node: Node, src: &str) -> (usize, usize) {
    let mut start = node;
    let mut cursor = node;
    while let Some(prev) = cursor.prev_sibling() {
        if !is_attachment(prev, src) {
            break;
        }
        if prev.kind() != "decorator" && has_blank_line_between(prev, cursor, src) {
            break;
        }
        start = prev;
        cursor = prev;
    }
    (start.start_byte(), start.start_position().row + 1)
}

/// The full byte span of a declaration including its attachments, `(start_byte, end_byte)`.
/// Companion to [`decl_span_with_attachments`] for callers that splice by byte offset.
pub fn decl_full_span(node: Node, src: &str) -> (usize, usize) {
    let (start, _) = decl_span_with_attachments(node, src);
    (start, node.end_byte())
}

/// True when the gap between two adjacent siblings contains an empty line.
fn has_blank_line_between(earlier: Node, later: Node, src: &str) -> bool {
    let (from, to) = (earlier.end_byte(), later.start_byte());
    if from >= to || to > src.len() {
        return false;
    }
    src[from..to].matches('\n').count() > 1
}

/// The decorators and doc comment attached to `node`, in source order.
///
/// Uses exactly the same walk as [`decl_span_with_attachments`], so the text returned and the span
/// removed by an edit can never disagree. When several doc comments precede a declaration the one
/// nearest to it wins.
pub fn attachments(node: Node, src: &str) -> (Vec<String>, Option<String>) {
    let mut decorators: Vec<String> = Vec::new();
    let mut doc: Option<String> = None;

    let mut cursor = node;
    while let Some(prev) = cursor.prev_sibling() {
        if !is_attachment(prev, src) {
            break;
        }
        if prev.kind() != "decorator" && has_blank_line_between(prev, cursor, src) {
            break;
        }
        match node_text(prev, src) {
            Some(text) if prev.kind() == "decorator" => decorators.push(text),
            // Walking backwards, the first doc comment we meet is the closest one.
            Some(text) if doc.is_none() => doc = Some(text),
            _ => {}
        }
        cursor = prev;
    }

    decorators.reverse();
    (decorators, doc)
}

/// The kind resq assigns to a declaration node, or `None` if the node is not a declaration.
pub fn declaration_kind(node: Node) -> Option<DeclarationKind> {
    match node.kind() {
        "let_declaration" => Some(DeclarationKind::Let),
        "type_declaration" => Some(DeclarationKind::Type),
        "module_declaration" => Some(DeclarationKind::Module),
        "external_declaration" => Some(DeclarationKind::External),
        "include_statement" => Some(DeclarationKind::Include),
        "open_statement" => Some(DeclarationKind::Open),
        _ => None,
    }
}

/// Read one declaration node into a [`Declaration`]. `path` is the **enclosing** module path — the
/// caller supplies it while walking, and this function never recurses.
///
/// Returns `None` for nodes that are not declarations.
pub fn declaration_from_node(node: Node, src: &str, path: &ModulePath) -> Option<Declaration> {
    let kind = declaration_kind(node)?;
    let (decorators, doc_comment) = attachments(node, src);
    let (_, start_line) = decl_span_with_attachments(node, src);

    let (names, binder_kind, type_annotation) = match kind {
        DeclarationKind::Let => let_declaration_parts(node, src),
        DeclarationKind::Type => (type_declaration_names(node, src), BinderKind::Simple, None),
        DeclarationKind::Module => (
            module_declaration_names(node, src),
            BinderKind::Simple,
            None,
        ),
        DeclarationKind::External => (
            named_children_of_kind(node, "value_identifier", src),
            BinderKind::Simple,
            type_annotation_text(node, src),
        ),
        // `open Belt.Array` / `include Belt.Array`: the single named child is the module path.
        DeclarationKind::Include | DeclarationKind::Open => (
            node.named_child(0)
                .and_then(|c| node_text(c, src))
                .into_iter()
                .collect(),
            BinderKind::Simple,
            None,
        ),
    };

    Some(Declaration {
        names,
        path: path.clone(),
        kind,
        binder_kind,
        decorators,
        type_annotation,
        doc_comment,
        start_line,
        end_line: node.end_position().row + 1,
    })
}

/// The `block` holding a module's members, for callers walking into nested modules.
///
/// `module_binding`'s `definition:` field is one of three things (SPEC §1): a `block` (a real
/// module body), a `module_identifier`/`module_identifier_path` (an *alias* — no members), or a
/// `functor`. Returns the block for a body or a functor body, and `None` for an alias.
pub fn module_body_block(module_declaration: Node<'_>) -> Option<Node<'_>> {
    let binding = named_child_of_kind(module_declaration, "module_binding")?;
    let definition = binding
        .child_by_field_name("definition")
        .or_else(|| binding.child_by_field_name("signature"))?;
    match definition.kind() {
        "block" => Some(definition),
        "functor" => definition.child_by_field_name("body"),
        _ => None,
    }
}

/// `module A = B.C` seen as an alias: `(name, target)`. `None` when the module has a body.
pub fn module_alias_parts(module_declaration: Node, src: &str) -> Option<(String, String)> {
    let binding = named_child_of_kind(module_declaration, "module_binding")?;
    let definition = binding.child_by_field_name("definition")?;
    if !matches!(
        definition.kind(),
        "module_identifier" | "module_identifier_path"
    ) {
        return None;
    }
    let name = node_text(binding.child_by_field_name("name")?, src)?;
    Some((name, node_text(definition, src)?))
}

// ---------------------------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------------------------

fn node_text(node: Node, src: &str) -> Option<String> {
    node.utf8_text(src.as_bytes()).ok().map(str::to_string)
}

fn named_child_of_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).find(|c| c.kind() == kind)
}

fn named_children_of_kind(node: Node, kind: &str, src: &str) -> Vec<String> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|c| c.kind() == kind)
        .filter_map(|c| node_text(c, src))
        .collect()
}

/// The text of a `type_annotation` child with the leading `:` stripped (SPEC §3.6).
fn type_annotation_text(node: Node, src: &str) -> Option<String> {
    let annotation = named_child_of_kind(node, "type_annotation")?;
    // The annotation's first named child is the type expression itself; falling back to trimming
    // the raw text keeps this working if a future grammar drops the wrapper.
    match annotation.named_child(0) {
        Some(type_expr) => node_text(type_expr, src),
        None => node_text(annotation, src)
            .map(|t| t.trim_start().trim_start_matches(':').trim().to_string()),
    }
}

/// Names, binder kind and annotation for a `let_declaration`.
///
/// A `let_declaration` may hold several `let_binding` children (`let a = 1 and b = 2`), and each
/// `let_binding` may hold several `pattern:` children (`let (a, b) as whole = pair`).
fn let_declaration_parts(node: Node, src: &str) -> (Vec<String>, BinderKind, Option<String>) {
    let mut names = Vec::new();
    let mut binder_kind = BinderKind::Simple;
    let mut annotation = None;

    let mut cursor = node.walk();
    let bindings: Vec<Node> = node
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "let_binding")
        .collect();

    for binding in bindings {
        if annotation.is_none() {
            annotation = type_annotation_text(binding, src);
        }
        for i in 0..binding.child_count() as u32 {
            if binding.field_name_for_child(i) != Some("pattern") {
                continue;
            }
            let Some(pattern) = binding.child(i) else {
                continue;
            };
            if pattern.kind() != "value_identifier" {
                binder_kind = BinderKind::Destructuring;
            }
            names.extend(bound_names(pattern, src));
        }
    }

    (names, binder_kind, annotation)
}

/// Every name a pattern binds, in source order, skipping `_` wildcards.
fn bound_names(pattern: Node, src: &str) -> Vec<String> {
    bound_name_spans(pattern, src)
        .into_iter()
        .filter_map(|(start, end)| src.get(start..end).map(str::to_owned))
        .collect()
}

/// Byte spans of every name a pattern binds, in source order, skipping `_` wildcards.
///
/// **This is the single implementation of SPEC §1 finding 8** — the `record_pattern`
/// field-vs-binder disambiguation. It returns spans rather than strings so that callers needing
/// source positions (`grep --definitions`, and later `refs`/`rename`) share this logic instead of
/// re-deriving it; [`bound_names`] is a thin text view over the same result. Two copies of this
/// algorithm drifting apart would make `rename` rewrite record *field names* as if they were
/// variables — silent file corruption. Do not reimplement it.
pub fn bound_name_spans(pattern: Node, src: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    collect_bound_name_spans(pattern, src, &mut out);
    out
}

fn collect_bound_name_spans(node: Node, src: &str, out: &mut Vec<(usize, usize)>) {
    if node.kind() == "value_identifier" {
        if let Some(text) = node_text(node, src)
            && text != "_"
        {
            out.push((node.start_byte(), node.end_byte()));
        }
        return;
    }

    // `record_pattern` is flat: `{x: a}` yields two sibling `value_identifier`s, the field name
    // and the binder, with no field to tell them apart. Only the binder is bound, so a child
    // followed by `:` is skipped in favour of the one after it.
    if node.kind() == "record_pattern" {
        let mut cursor = node.walk();
        let children: Vec<Node> = node.named_children(&mut cursor).collect();
        let mut i = 0;
        while i < children.len() {
            let child = children[i];
            let renamed = children.get(i + 1).is_some_and(|next| {
                child.end_byte() <= next.start_byte()
                    && src[child.end_byte()..next.start_byte()]
                        .trim_start()
                        .starts_with(':')
            });
            if renamed {
                collect_bound_name_spans(children[i + 1], src, out);
                i += 2;
            } else {
                collect_bound_name_spans(child, src, out);
                i += 1;
            }
        }
        return;
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_bound_name_spans(child, src, out);
    }
}

/// `type a = int and b = string` declares two names from one `type_declaration`.
fn type_declaration_names(node: Node, src: &str) -> Vec<String> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|c| c.kind() == "type_binding")
        .filter_map(|b| b.child_by_field_name("name"))
        .filter_map(|n| node_text(n, src))
        .collect()
}

/// `module_binding.name` may be a `type_identifier` rather than a `module_identifier` (SPEC §1),
/// so take the `name:` field whatever kind it holds.
fn module_declaration_names(node: Node, src: &str) -> Vec<String> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|c| c.kind() == "module_binding")
        .filter_map(|b| b.child_by_field_name("name"))
        .filter_map(|n| node_text(n, src))
        .collect()
}
