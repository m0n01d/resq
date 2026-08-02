//! `resq add open`, `resq add alias`, `resq rm open` — SPEC §3.2, §4.
//!
//! WAVE 2 (agent A8) owns this file. All three commands are WRITE commands and obey the
//! write-safety invariant (SPEC §2) via [`parser::ensure_clean_parse`] (step 1) and
//! [`writer::validate_output`] / [`writer::validated_write`] (steps 2–3): refuse a file that is
//! already broken, build the new buffer, refuse to write it if it would not re-parse, and only
//! then write atomically. Every success prints `ok` on stdout.
//!
//! **Scope note (v1):** `add open` and `add alias` only insert at the file root, and `rm open`
//! only removes file-root `open` statements. ReScript allows `open`/`module A = B` inside a
//! nested module body too, but SPEC §4's command surface addresses `<Module>` by literal name, not
//! by dot-path, so there is no way to say "the `open` inside `Inner`" — v1 does not attempt it.
//! An `open`/alias inside a nested module is invisible to these three commands; this is a
//! deliberate limitation, not a bug.
//!
//! ## `rm open`'s safety heuristic — read before trusting it
//!
//! Removing an `open` can turn a previously-unqualified reference into an unresolved name. resq
//! has no type information and does not know what any module exports (SPEC §3.2), so it cannot
//! attribute a given unqualified identifier to the specific module whose `open` is being removed.
//! Given that, the heuristic implemented here is deliberately blunt and conservative:
//!
//! 1. Scan the **whole file** (not just the vicinity of the `open` being removed) for
//!    **candidate free identifiers**: bare `value_identifier` references that (a) are not part of
//!    a qualified path (`Foo.bar` — a `value_identifier_path` node is skipped entirely, since a
//!    qualified reference never depends on any `open`) and (b) are not bound by an **enclosing**
//!    pattern, function parameter, `let`, or `external` declaration — see "scope-awareness" below.
//! 2. If that scan finds **any** candidate at all, refuse to remove **any** of the requested
//!    opens, list every candidate with its `line:col`, and say plainly that resq cannot prove
//!    they are unrelated to the open being removed. `--force` skips the scan entirely.
//! 3. If the scan finds **nothing** (nothing in the file has an unqualified, unbound reference),
//!    every requested open is safe to remove.
//!
//! **Scope-awareness (the load-bearing correctness property).** A name bound in one function must
//! never mask a free reference in a *different* function — that would be under-approximating in
//! the dangerous direction (silently allowing an unsafe removal), so it is not just an accuracy
//! nicety, it is the one invariant this scan is not allowed to violate. Concretely:
//! `let map = 1` inside `let f = () => { let map = 1; map }` must not hide a genuinely free `map`
//! inside a sibling `let g = () => map`. [`scan_free_identifiers`] tracks a **stack of binding
//! frames**, not one whole-file set: entering a `block`, a `function`'s parameters+body, or a
//! `switch_match`'s pattern+guard+body pushes a fresh frame that is popped again once that
//! construct is done, so those bindings are only visible to code actually inside them. A `let`
//! binding (or `external`) extends the *current* frame instead of pushing a new one, matching
//! ReScript's real "visible for the rest of this block" scoping. A reference is "bound" only if
//! some frame **currently on the stack** (i.e. an actual lexical ancestor of the reference)
//! contains its name — never a frame that has already been popped.
//!
//! This is still not a full scope analysis (see the explicit non-goals below), but it is sound in
//! the specific direction that matters: where it is cheap or ambiguous, it approximates toward
//! **free**, never toward **bound**, which is the safe side for a removal-safety check.
//!
//! **Over-approximates (refuses removals that would actually be fine):**
//! - It cannot attribute a free identifier to *this specific* open, so a file with one unrelated
//!   free identifier anywhere blocks removal of *every* open, not just the one that (if any)
//!   provides that identifier. `--force` is the intended escape hatch for a human/agent who has
//!   checked and is confident.
//! - A name that is only ever available via ReScript's implicit Stdlib prelude (not through any
//!   explicit `open` in the file) looks exactly like a genuinely free identifier and triggers a
//!   refusal even though removing the target `open` would not affect it.
//!
//! **Under-approximates (could still, in narrow cases, wrongly allow an unsafe removal):**
//! - Only unqualified **value** identifiers are scanned. Bare **constructors** and **type names**
//!   (e.g. `Increment` instead of `Types.Increment`, made legal by `open Types`) are a different
//!   grammar node kind and are not modeled at all — removing an open that only a bare constructor
//!   or type reference depends on is not detected.
//! - JSX component tags (`<Foo />`) are not modeled (SPEC §3.10 — JSX-aware refactoring is out of
//!   scope for v1); a JSX reference that depends on the open is invisible to the scan.
//! - A `~label: ty = default` labeled-parameter default value is (for implementation simplicity)
//!   treated as part of the parameter's binding pattern rather than as an evaluated expression, so
//!   an identifier appearing *only* inside such a default value is misclassified as "bound" —
//!   **now scoped to just that one function's frame** (fixed from the original whole-file version
//!   of this bug), so the blast radius is a single function rather than the entire file, but it is
//!   not eliminated. This construct does not appear in any v1 fixture; documented here rather than
//!   silently accepted.
//! - Within one `let a = 1 and b = 2` (mutually non-recursive by ReScript syntax, but not
//!   distinguished from `let rec … and …` at this grammar's node-kind level), `a` is added to the
//!   current frame before `b`'s body is visited, so `b`'s body sees `a` as bound. This mirrors
//!   `let rec` semantics rather than plain `let … and …` semantics in the rare case they differ;
//!   it does not cross a function boundary and is not the class of bug this fix targets.
//!
//! Bottom line: an empty scan result is a strong "safe to remove" signal. A non-empty scan result
//! is a weak "maybe unsafe" signal that over-refuses by design — treat `--force` as "I checked by
//! hand and I'm sure", not as "resq was just being paranoid".

use crate::cli::{AddAlias, AddOpen, RmOpen};
use crate::{DeclarationKind, parser, writer};
use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use tree_sitter::Node;

// ============================================================================================
// add open
// ============================================================================================

pub fn run_add_open(args: AddOpen) -> Result<()> {
    let file = args.file;
    let source = std::fs::read_to_string(&file)
        .with_context(|| format!("failed to read {}", file.display()))?;
    let tree = parser::ensure_clean_parse(&source, &file)?;

    let existing: HashSet<String> = current_opens(tree.root_node(), &source)
        .into_iter()
        .collect();

    let mut to_add: Vec<String> = Vec::new();
    let mut already: Vec<String> = Vec::new();
    for module in dedup_preserve_order(args.modules) {
        if existing.contains(&module) || to_add.contains(&module) {
            already.push(module);
        } else {
            to_add.push(module);
        }
    }

    if to_add.is_empty() {
        println!("already open, no changes: {}", already.join(", "));
        return Ok(());
    }

    let offset = insertion_offset(tree.root_node(), &source);
    let mut block = String::new();
    for module in &to_add {
        block.push_str("open ");
        block.push_str(module);
        block.push('\n');
    }
    let new_source = splice(&source, offset, offset, &block);

    writer::validated_write(&file, &new_source, "add open")?;
    if !already.is_empty() {
        eprintln!("already open, skipped: {}", already.join(", "));
    }
    println!("ok");
    Ok(())
}

// ============================================================================================
// add alias
// ============================================================================================

pub fn run_add_alias(args: AddAlias) -> Result<()> {
    let file = args.file;

    // Parse every `<Name>=<Module>` argument up front — fail fast, before touching the file, on
    // a malformed argument.
    let mut pairs: Vec<(String, String)> = Vec::new();
    for raw in &args.aliases {
        pairs.push(parse_alias_arg(raw)?);
    }

    let source = std::fs::read_to_string(&file)
        .with_context(|| format!("failed to read {}", file.display()))?;
    let tree = parser::ensure_clean_parse(&source, &file)?;

    let existing: HashSet<(String, String)> = current_aliases(tree.root_node(), &source)
        .into_iter()
        .collect();

    let mut to_add: Vec<(String, String)> = Vec::new();
    let mut already: Vec<String> = Vec::new();
    for (name, target) in pairs {
        let pair = (name.clone(), target.clone());
        if existing.contains(&pair) || to_add.contains(&pair) {
            already.push(format!("{name}={target}"));
        } else {
            to_add.push(pair);
        }
    }

    if to_add.is_empty() {
        println!("already aliased, no changes: {}", already.join(", "));
        return Ok(());
    }

    let offset = insertion_offset(tree.root_node(), &source);
    let mut block = String::new();
    for (name, target) in &to_add {
        block.push_str("module ");
        block.push_str(name);
        block.push_str(" = ");
        block.push_str(target);
        block.push('\n');
    }
    let new_source = splice(&source, offset, offset, &block);

    writer::validated_write(&file, &new_source, "add alias")?;
    if !already.is_empty() {
        eprintln!("already aliased, skipped: {}", already.join(", "));
    }
    println!("ok");
    Ok(())
}

/// Split `"Name=Module"` into `(name, target)`. Neither side may be empty.
fn parse_alias_arg(raw: &str) -> Result<(String, String)> {
    let (name, target) = raw
        .split_once('=')
        .with_context(|| format!("malformed alias `{raw}`: expected <Name>=<Module>"))?;
    if name.is_empty() || target.is_empty() {
        bail!("malformed alias `{raw}`: expected <Name>=<Module>, both sides non-empty");
    }
    Ok((name.to_string(), target.to_string()))
}

// ============================================================================================
// rm open
// ============================================================================================

pub fn run_rm_open(args: RmOpen) -> Result<()> {
    let file = args.file;
    let source = std::fs::read_to_string(&file)
        .with_context(|| format!("failed to read {}", file.display()))?;
    let tree = parser::ensure_clean_parse(&source, &file)?;
    let root = tree.root_node();

    let requested = dedup_preserve_order(args.modules);
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut found: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();

    for module in &requested {
        match find_open_node(root, &source, module) {
            Some(node) => {
                spans.push(removal_span(node, &source));
                found.push(module.clone());
            }
            None => missing.push(module.clone()),
        }
    }

    if found.is_empty() {
        println!(
            "no matching `open` found, no changes: {}",
            requested.join(", ")
        );
        return Ok(());
    }

    if !args.force {
        let free = scan_free_identifiers(root, &source);
        if !free.is_empty() {
            let listing = free
                .iter()
                .map(|f| format!("{} ({}:{})", f.name, f.line, f.col))
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "refusing to remove `open {}` from {}: the file has unqualified reference(s) \
                 that resq cannot prove are unrelated to this open (no type information — see \
                 `resq::imports` module docs for the exact heuristic): {listing}. Pass --force \
                 to remove anyway.",
                found.join(", "),
                file.display()
            );
        }
    }

    let new_source = remove_spans(&source, &spans);
    writer::validated_write(&file, &new_source, "rm open")?;
    if !missing.is_empty() {
        eprintln!("not found, skipped: {}", missing.join(", "));
    }
    println!("ok");
    Ok(())
}

/// The top-level `open_statement` node whose target text equals `module`, if any.
fn find_open_node<'a>(root: Node<'a>, src: &str, module: &str) -> Option<Node<'a>> {
    let mut cursor = root.walk();
    root.named_children(&mut cursor).find(|child| {
        parser::declaration_kind(*child) == Some(DeclarationKind::Open)
            && open_target_text(*child, src).as_deref() == Some(module)
    })
}

/// The byte span to delete for a matched `open` node: its declaration span *with* attachments
/// (SPEC §1 finding 1 — an `open` can carry its own doc comment) through the end of its own line,
/// consuming exactly one trailing newline so removal never leaves a blank line behind.
fn removal_span(node: Node, src: &str) -> (usize, usize) {
    let (start, _) = parser::decl_span_with_attachments(node, src);
    let end = node.end_byte();
    let end = if src.as_bytes().get(end) == Some(&b'\n') {
        end + 1
    } else {
        end
    };
    (start, end)
}

/// Delete every `(start, end)` byte span from `src`. Spans must be non-overlapping; order does
/// not matter, this sorts them first.
fn remove_spans(src: &str, spans: &[(usize, usize)]) -> String {
    let mut sorted = spans.to_vec();
    sorted.sort_by_key(|&(s, _)| s);
    let mut out = String::with_capacity(src.len());
    let mut cursor = 0;
    for (start, end) in sorted {
        if start < cursor {
            continue; // overlapping/duplicate span — already consumed
        }
        out.push_str(&src[cursor..start]);
        cursor = end;
    }
    out.push_str(&src[cursor..]);
    out
}

// ============================================================================================
// Shared: current opens/aliases, insertion point
// ============================================================================================

/// Every file-root `open X` target, in source order (duplicates included — callers dedup).
fn current_opens(root: Node, src: &str) -> Vec<String> {
    let mut cursor = root.walk();
    root.named_children(&mut cursor)
        .filter(|c| parser::declaration_kind(*c) == Some(DeclarationKind::Open))
        .filter_map(|c| open_target_text(c, src))
        .collect()
}

fn open_target_text(node: Node, src: &str) -> Option<String> {
    node.named_child(0)
        .and_then(|c| c.utf8_text(src.as_bytes()).ok())
        .map(str::to_string)
}

/// Every file-root `module Name = Target` alias, as `(name, target)`, in source order.
fn current_aliases(root: Node, src: &str) -> Vec<(String, String)> {
    let mut cursor = root.walk();
    root.named_children(&mut cursor)
        .filter(|c| parser::declaration_kind(*c) == Some(DeclarationKind::Module))
        .filter_map(|c| parser::module_alias_parts(c, src))
        .collect()
}

/// Where a new `open`/alias block should be inserted: right after the last existing file-root
/// `open` or alias, or — if there are none — at the very top of the file, after a leading
/// file-level doc comment (one that is not itself the attached doc comment of the first real
/// declaration, i.e. separated from it by a blank line, or which is the only thing in the file).
fn insertion_offset(root: Node, src: &str) -> usize {
    let mut cursor = root.walk();
    let children: Vec<Node> = root.named_children(&mut cursor).collect();

    let mut anchor_end: Option<usize> = None;
    for &child in &children {
        let is_open = parser::declaration_kind(child) == Some(DeclarationKind::Open);
        let is_alias = parser::declaration_kind(child) == Some(DeclarationKind::Module)
            && parser::module_alias_parts(child, src).is_some();
        if is_open || is_alias {
            anchor_end = Some(child.end_byte());
        }
    }

    if let Some(end) = anchor_end {
        return skip_to_next_line(src, end);
    }

    if let Some(&first) = children.first()
        && parser::is_doc_comment(first, src)
    {
        let detached = match children.get(1) {
            Some(&second) => blank_line_between(src, first.end_byte(), second.start_byte()),
            None => true,
        };
        if detached {
            return skip_to_next_line(src, first.end_byte());
        }
    }

    0
}

/// The byte offset of the start of the line following `offset` (i.e. right after the next `\n`
/// at/after `offset`), or `src.len()` if there is none.
fn skip_to_next_line(src: &str, offset: usize) -> usize {
    match src[offset.min(src.len())..].find('\n') {
        Some(rel) => offset + rel + 1,
        None => src.len(),
    }
}

fn blank_line_between(src: &str, from: usize, to: usize) -> bool {
    if from >= to || to > src.len() {
        return false;
    }
    src[from..to].matches('\n').count() > 1
}

fn splice(src: &str, remove_start: usize, remove_end: usize, insert: &str) -> String {
    let mut out = String::with_capacity(src.len() + insert.len());
    out.push_str(&src[..remove_start]);
    out.push_str(insert);
    out.push_str(&src[remove_end..]);
    out
}

fn dedup_preserve_order(items: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert(item.clone()))
        .collect()
}

// ============================================================================================
// Free-identifier scan (see module doc comment for the exact contract)
// ============================================================================================

struct FreeRef {
    name: String,
    line: usize,
    col: usize,
}

/// A stack of binding-name sets, innermost scope last. A reference is bound only if some frame
/// **currently on the stack** contains its name — a popped frame can never mask anything again.
type ScopeStack = Vec<HashSet<String>>;

fn is_bound(stack: &ScopeStack, name: &str) -> bool {
    stack.iter().any(|frame| frame.contains(name))
}

/// Add every name [`parser::bound_name_spans`] finds in `pattern_node` to the **innermost**
/// (current) frame. Used for bindings that extend the enclosing scope rather than introducing
/// their own (`let`, `external`) — see the module doc comment.
fn bind_into_current_frame(stack: &mut ScopeStack, pattern_node: Node, src: &str) {
    let frame = stack.last_mut().expect("scope stack is never empty");
    for (s, e) in parser::bound_name_spans(pattern_node, src) {
        if let Some(text) = src.get(s..e) {
            frame.insert(text.to_string());
        }
    }
}

/// Every candidate free (unqualified, unbound) `value_identifier` reference in the file, deduped
/// by name (first occurrence wins), in source order. See the module doc comment for exactly what
/// this over- and under-approximates.
fn scan_free_identifiers(root: Node, src: &str) -> Vec<FreeRef> {
    let mut stack: ScopeStack = vec![HashSet::new()]; // the file-root frame
    let mut refs: Vec<(String, usize, usize)> = Vec::new();
    walk_for_refs(root, src, &mut stack, &mut refs);

    let mut seen = HashSet::new();
    refs.into_iter()
        .filter(|(name, ..)| seen.insert(name.clone()))
        .map(|(name, line, col)| FreeRef { name, line, col })
        .collect()
}

/// One recursive, **scope-aware** walk that both tracks bound names and collects candidate free
/// references. "Scope-aware" is the load-bearing word: a name bound inside one function must
/// never mask a free reference inside a different function (or a different `switch` arm, or a
/// sibling `block`) — see the module doc comment's "scope-awareness" section for why this is a
/// correctness requirement, not a nicety.
///
/// Three node kinds get dedicated handling because they introduce a scope that must be popped
/// again once the construct ends: `block` (module bodies, do-expressions, if/else branches),
/// `function` (parameters are visible only in that function's own body), and `switch_match` (a
/// pattern's binders are visible only in that arm's own guard + body). Everything else dispatches
/// generically on the grammar's own field names: a child reached through a `pattern` field
/// (chiefly `let_binding.pattern`, since `switch_match`/`function` are handled explicitly before
/// the generic case is reached) is a *binding* context whose names extend the **current** frame —
/// this is what makes `let`/`external` visible for the rest of their enclosing block, matching
/// real ReScript scoping, while still not leaking out of that block. A `value_identifier_path` (a
/// qualified reference like `Belt.Array.length`) is skipped whole, since it never depends on any
/// `open`.
fn walk_for_refs(
    node: Node,
    src: &str,
    stack: &mut ScopeStack,
    refs: &mut Vec<(String, usize, usize)>,
) {
    if node.kind() == "value_identifier_path" {
        return;
    }
    if node.kind() == "value_identifier" {
        if let Some(text) = node.utf8_text(src.as_bytes()).ok().filter(|t| *t != "_")
            && !is_bound(stack, text)
        {
            let (line, col) = parser::byte_offset_to_line_col(src, node.start_byte());
            refs.push((text.to_string(), line, col));
        }
        return;
    }
    // `external evalRaw: string => unit = "eval"` binds `evalRaw` as a plain, un-fielded
    // `value_identifier` child (SPEC §1: `external_declaration` children are just
    // `value_identifier, type_annotation, string`, no field names) — without this, the walker
    // would treat the external's own name as a free reference to itself. It extends the current
    // frame, exactly like a `let`.
    if node.kind() == "external_declaration" {
        for i in 0..node.child_count() as u32 {
            let Some(child) = node.child(i) else { continue };
            if !child.is_named() {
                continue;
            }
            if child.kind() == "value_identifier" {
                if let Ok(text) = child.utf8_text(src.as_bytes()) {
                    stack
                        .last_mut()
                        .expect("scope stack is never empty")
                        .insert(text.to_string());
                }
            } else {
                walk_for_refs(child, src, stack, refs);
            }
        }
        return;
    }

    // A `block` is a fresh lexical scope: a `let` inside it must not leak to whatever follows the
    // block in the *enclosing* scope (SPEC's fixture doesn't need this for correctness of the
    // required test cases, but it is what keeps this from re-introducing the whole-file-union bug
    // one level down, e.g. inside an `if`/`else` branch or a do-expression).
    if node.kind() == "block" {
        stack.push(HashSet::new());
        walk_children_generic(node, src, stack, refs);
        stack.pop();
        return;
    }

    // `function`: parameters are visible only inside this function's own body, never outside it
    // and never in a sibling function. This is the exact construct in the reported repro
    // (`let f = () => { let map = 1; map }` vs. sibling `let g = () => map`).
    if node.kind() == "function" {
        stack.push(HashSet::new());
        if let Some(param) = node.child_by_field_name("parameter") {
            bind_into_current_frame(stack, param, src);
        }
        if let Some(params) = node.child_by_field_name("parameters") {
            bind_into_current_frame(stack, params, src);
        }
        if let Some(body) = node.child_by_field_name("body") {
            walk_for_refs(body, src, stack, refs);
        }
        // `return_type` is a type expression (type_identifier, not value_identifier) — nothing
        // for this scan to find there, so it is not walked.
        stack.pop();
        return;
    }

    // `switch_match`: a pattern's binders (`| Decrement(n) if n > 0 => …`) are visible only in
    // that arm's own guard + body, never in a sibling arm and never after the switch.
    if node.kind() == "switch_match" {
        stack.push(HashSet::new());
        for i in 0..node.child_count() as u32 {
            let Some(child) = node.child(i) else { continue };
            if child.is_named() && node.field_name_for_child(i) == Some("pattern") {
                bind_into_current_frame(stack, child, src);
            }
        }
        for i in 0..node.child_count() as u32 {
            let Some(child) = node.child(i) else { continue };
            if !child.is_named() || node.field_name_for_child(i) == Some("pattern") {
                continue;
            }
            // Whatever is left (the optional `guard`, and `body`) is evaluated under the
            // pattern's binders.
            walk_for_refs(child, src, stack, refs);
        }
        stack.pop();
        return;
    }

    walk_children_generic(node, src, stack, refs);
}

/// The generic per-child dispatch shared by every node kind without its own scope-management
/// rule: a child reached through a `pattern` field extends the *current* frame (a `let_binding`,
/// the common case once `function`/`switch_match` have already been handled above); everything
/// else recurses normally, in the same scope.
fn walk_children_generic(
    node: Node,
    src: &str,
    stack: &mut ScopeStack,
    refs: &mut Vec<(String, usize, usize)>,
) {
    for i in 0..node.child_count() as u32 {
        let Some(child) = node.child(i) else { continue };
        if !child.is_named() {
            continue;
        }
        match node.field_name_for_child(i) {
            Some("pattern") | Some("parameter") | Some("parameters") => {
                bind_into_current_frame(stack, child, src);
            }
            _ => walk_for_refs(child, src, stack, refs),
        }
    }
}
