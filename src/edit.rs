//! `resq set decl`, `resq patch`, `resq rm decl` — the single-file write commands (SPEC §4).
//!
//! Every public entry point here obeys the write-safety invariant (SPEC §2), in this exact order:
//!
//! 1. [`parser::ensure_clean_parse`] the input — a file with pre-existing ERROR nodes is never
//!    touched, and the error names the file and the first fault's `line:col`.
//! 2. build the new buffer in memory, then hand it to [`writer::validated_write`], which re-parses
//!    it and refuses to write anything if the result would not parse. On refusal the on-disk file
//!    is byte-for-byte unchanged, because nothing has been written yet.
//! 3. print `ok`.
//!
//! Step 2 is not belt-and-braces: it is what catches bugs in *our own splicing*, not just bad user
//! input. Do not add a write path that bypasses `writer::validated_write`.
//!
//! ## Two deliberate refusals
//!
//! **The `.resi` sync guard (SPEC §3.3).** resq has no `expose`/`unexpose` commands: a `.resi`
//! parses with the same nodes as a `.res` (SPEC §1 finding 5), so it is edited with these same
//! three commands. What survives of Elm's `exposing (...)` is one invariant — removing a
//! declaration from a `.res` whose sibling `.resi` still names it would leave a project that does
//! not compile, so [`rm_decl`] refuses and names the orphans. See [`check_resi_sync`].
//!
//! **Partial multi-name removal.** `Declaration::names` is a `Vec` (SPEC §3.7): `let (a, b) = pair`
//! and `let a = 1 and b = 2` each bind two names through *one* declaration node, and there is no
//! smaller thing to delete. Rather than silently unbind `b` when asked to remove `a`, both
//! [`rm_decl`] and [`set_decl`] refuse unless every name the declaration binds is accounted for.
//! See [`ensure_all_names_covered`].

use crate::cli::{RmDecl, SetDecl};
use crate::{Declaration, DeclarationKind, ModulePath};
use crate::{parser, project, writer};
use anyhow::{Context, Result, bail};
use std::fs;
use std::io::{IsTerminal, Read};
use std::path::Path;
use tree_sitter::Node;

// =============================================================================================
// CLI entry points (wired from main.rs). Each prints `ok` on success; every error is non-zero.
// =============================================================================================

/// `resq set decl <FILE> [--name <PATH>] [--content <SRC> | stdin]`.
pub fn run_set_decl(args: SetDecl) -> Result<()> {
    let content = choose_content(args.content, read_stdin_if_piped()?)?;
    set_decl(&args.file, args.name.as_deref(), &content)?;
    println!("ok");
    Ok(())
}

/// `resq patch <FILE> <PATH> --old <STR> --new <STR>`.
pub fn run_patch(file: &Path, path: &str, old: &str, new: &str) -> Result<()> {
    patch(file, path, old, new)?;
    println!("ok");
    Ok(())
}

/// `resq rm decl <FILE> <PATH>...`.
pub fn run_rm_decl(args: RmDecl) -> Result<()> {
    rm_decl(&args.file, &args.names)?;
    println!("ok");
    Ok(())
}

// =============================================================================================
// File-level operations — read, transform, validate, write. Tests drive these.
// =============================================================================================

/// Upsert the declaration at `name`: replace it if it exists, append it if it does not.
///
/// `name` may be omitted, in which case the target path is the root-level name the content itself
/// declares. When both are present they must agree — a mismatch is an error, never a silent
/// rename.
pub fn set_decl(file: &Path, name: Option<&str>, content: &str) -> Result<()> {
    let src = read_source(file)?;
    let updated = set_decl_source(&src, file, name, content)?;
    writer::validated_write(file, &updated, "set decl")
}

/// Exact find-and-replace of `old` with `new`, scoped to the source span of the declaration at
/// `path` (its decorators and doc comment included).
///
/// Must match **exactly once**: zero matches and two-or-more matches are both errors. This
/// strictness is the point of the command — an agent that patches the wrong occurrence corrupts
/// code silently, and a patch that matched nothing would report success for a no-op.
pub fn patch(file: &Path, path: &str, old: &str, new: &str) -> Result<()> {
    let src = read_source(file)?;
    let updated = patch_source(&src, file, path, old, new)?;
    writer::validated_write(file, &updated, "patch")
}

/// Remove each declaration at `paths`, **including its decorators and doc comment**.
///
/// Enforces the `.resi` sync guard before writing anything (SPEC §3.3), and refuses a removal that
/// would silently unbind a name the caller did not ask to remove (SPEC §3.7).
pub fn rm_decl(file: &Path, paths: &[String]) -> Result<()> {
    let src = read_source(file)?;
    let targets = parse_paths(paths);
    // The guard runs first: it is a refusal about the *pair* of files, and the caller should hear
    // about an orphaned signature rather than an unrelated resolution error inside the `.res`.
    check_resi_sync(file, &targets)?;
    let updated = rm_decl_source(&src, file, &targets)?;
    writer::validated_write(file, &updated, "rm decl")
}

fn read_source(file: &Path) -> Result<String> {
    fs::read_to_string(file).with_context(|| format!("failed to read {}", file.display()))
}

// =============================================================================================
// Pure transforms — `source in, source out`. These do step 1 of the invariant; the callers above
// do steps 2 and 3.
// =============================================================================================

/// The buffer that `set decl` would write. See [`set_decl`].
pub fn set_decl_source(
    src: &str,
    file: &Path,
    name: Option<&str>,
    content: &str,
) -> Result<String> {
    let tree = parser::ensure_clean_parse(src, file)?;

    let content = content.trim_end();
    if content.trim().is_empty() {
        bail!("resq set decl: content is empty");
    }
    let content_names = content_declaration_names(content)?;

    let target = match name {
        Some(n) => {
            let path = ModulePath::parse(n);
            if path.is_empty() {
                bail!(
                    "resq set decl: --name must be a dot-path such as `helper` or `Inner.helper`"
                );
            }
            path
        }
        // No `--name`: the content names itself, at the file root.
        None => ModulePath::root().child(content_names[0].clone()),
    };

    // A mismatch here means the user is one typo away from creating a second declaration under a
    // name that does not exist in the content — or, worse, silently renaming. Refuse both.
    let leaf = target
        .leaf()
        .expect("target path is non-empty, so it has a leaf");
    if !content_names.iter().any(|n| n == leaf) {
        bail!(
            "resq set decl: content declares `{}` but --name is `{target}`; \
             refusing to rename silently",
            content_names.join("`, `")
        );
    }

    let outline = Outline::of(tree.root_node(), src);
    let hits: Vec<&Located> = outline
        .decls
        .iter()
        .filter(|l| l.decl.is_at(&target))
        .collect();

    match hits.as_slice() {
        [] => append_declaration(src, &outline, file, &target, content),
        [hit] => {
            // Replacing `let (a, b) = pair` with a binding for only `a` would drop `b` — the same
            // hazard `rm decl` refuses (SPEC §3.7).
            let dropped: Vec<&String> = hit
                .decl
                .names
                .iter()
                .filter(|n| !content_names.contains(n))
                .collect();
            if !dropped.is_empty() {
                bail!(
                    "resq set decl: the declaration at `{target}` in {} also binds `{}`, \
                     which the new content does not; replacing it would silently unbind {}. \
                     Provide content binding every name.",
                    file.display(),
                    dropped
                        .iter()
                        .map(|n| n.as_str())
                        .collect::<Vec<_>>()
                        .join("`, `"),
                    if dropped.len() == 1 { "it" } else { "them" }
                );
            }
            let (start, end) = parser::decl_full_span(hit.node, src);
            let indent = line_indent_at(src, start);
            Ok(splice(
                src,
                start,
                end,
                &indent_continuation_lines(content, &indent),
            ))
        }
        many => bail!(
            "resq set decl: `{target}` is ambiguous in {} ({} matching declarations)",
            file.display(),
            many.len()
        ),
    }
}

/// The buffer that `patch` would write. See [`patch`].
pub fn patch_source(src: &str, file: &Path, path: &str, old: &str, new: &str) -> Result<String> {
    let tree = parser::ensure_clean_parse(src, file)?;
    if old.is_empty() {
        bail!("resq patch: --old must not be empty");
    }

    let path = ModulePath::parse(path);
    let outline = Outline::of(tree.root_node(), src);
    let hit = outline.resolve(file, &path, "patch")?;

    let (start, end) = parser::decl_full_span(hit.node, src);
    let scope = &src[start..end];
    let matches: Vec<usize> = scope.match_indices(old).map(|(i, _)| i).collect();
    match matches.as_slice() {
        [] => bail!(
            "resq patch: `--old` text not found within `{path}` in {} \
             (searched lines {}..={})",
            file.display(),
            hit.decl.start_line,
            hit.decl.end_line
        ),
        [at] => Ok(splice(src, start + at, start + at + old.len(), new)),
        many => bail!(
            "resq patch: `--old` matches {} times within `{path}` in {}; \
             it must match exactly once — narrow the search string",
            many.len(),
            file.display()
        ),
    }
}

/// The buffer that `rm decl` would write, **without** the `.resi` sync guard — that guard needs
/// the sibling file from disk and lives in [`check_resi_sync`], which [`rm_decl`] runs first.
pub fn rm_decl_source(src: &str, file: &Path, paths: &[ModulePath]) -> Result<String> {
    let tree = parser::ensure_clean_parse(src, file)?;
    if paths.is_empty() {
        bail!("resq rm decl: at least one dot-path is required");
    }
    let outline = Outline::of(tree.root_node(), src);

    let mut spans: Vec<(usize, usize)> = Vec::new();
    for path in paths {
        let hit = outline.resolve(file, path, "rm decl")?;
        ensure_all_names_covered(file, &hit.decl, path, paths)?;
        // `parser::decl_span_with_attachments` (inside `decl_full_span`) is what makes this
        // correct: decorators and the doc comment are *siblings* of the declaration (SPEC §1
        // finding 1), so deleting only `node`'s own span would orphan `@genType` onto whatever
        // declaration comes next and silently change its meaning.
        spans.push(parser::decl_full_span(hit.node, src));
    }

    // Two paths can name the same declaration (`first` and `second` of `let (first, second) = …`),
    // and removing the same span twice would corrupt the buffer.
    spans.sort_unstable();
    spans.dedup();
    spans.reverse();

    let mut out = src.to_string();
    for (start, end) in spans {
        out = remove_span(&out, start, end);
    }
    Ok(out)
}

/// SPEC §3.3 — the `.resi` sync guard.
///
/// Removing `polyColor` from `View.res` while `View.resi` still declares `polyColor` produces a
/// project that does not compile. resq has no `unexpose` command to fix that up (deliberately: see
/// the module docs), so the only safe behaviour is to refuse and name the orphaned entries. The
/// user edits the `.resi` with these same commands and then retries.
///
/// Only `.res` files are guarded: editing a `.resi` directly is the supported way to change a
/// module's public surface, and guarding it would make that impossible.
pub fn check_resi_sync(file: &Path, paths: &[ModulePath]) -> Result<()> {
    if file.extension().and_then(|e| e.to_str()) != Some("res") {
        return Ok(());
    }
    let Some(sibling) = project::find_sibling(file) else {
        return Ok(());
    };

    let sibling_src = read_source(&sibling)?;
    let tree = parser::parse(&sibling_src)
        .with_context(|| format!("failed to parse {}", sibling.display()))?;
    // A `.resi` we cannot parse is a `.resi` we cannot check, and an unverifiable invariant is a
    // refusal, not a pass.
    if tree.root_node().has_error() {
        let where_ = match parser::first_error_location(&tree, &sibling_src) {
            Some((line, col)) => format!(" at {line}:{col}"),
            None => String::new(),
        };
        bail!(
            "refusing to edit {}: its interface file {} does not parse{where_}, \
             so the .resi sync guard cannot be verified",
            file.display(),
            sibling.display()
        );
    }

    let outline = Outline::of(tree.root_node(), &sibling_src);
    let orphans: Vec<String> = paths
        .iter()
        .filter(|path| outline.decls.iter().any(|l| l.decl.is_at(path)))
        .map(ModulePath::to_string)
        .collect();

    if !orphans.is_empty() {
        bail!(
            "refusing to remove `{}` from {}: {} still declares {}. \
             Remove the signature first (`resq rm decl {} {}`), then retry.",
            orphans.join("`, `"),
            file.display(),
            sibling.display(),
            if orphans.len() == 1 {
                "it"
            } else {
                "those names"
            },
            sibling.display(),
            orphans.join(" ")
        );
    }
    Ok(())
}

/// SPEC §3.7 — one declaration node can bind several names, and there is no way to delete "half"
/// of it. `let (a, b) = pair` removed as `rm decl a` would silently unbind `b`.
///
/// **Decision: refuse.** The removal is allowed only when every name the declaration binds appears
/// among the requested paths, i.e. `resq rm decl Main.res first second` works and
/// `resq rm decl Main.res first` does not. This is deliberately stricter than SPEC §3.7's "refuse
/// if other bound names are still referenced": that phrasing needs project-wide reference
/// resolution (`refs`, agent A7), which is not available on this path, and guessing in the
/// unavailable direction would mean deleting code. The error message names the missing paths, so
/// the fix is a copy-paste away.
fn ensure_all_names_covered(
    file: &Path,
    decl: &Declaration,
    requested: &ModulePath,
    all_requested: &[ModulePath],
) -> Result<()> {
    let missing: Vec<String> = decl
        .names
        .iter()
        .map(|n| decl.path.child(n))
        .filter(|p| !all_requested.contains(p))
        .map(|p| p.to_string())
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    bail!(
        "refusing to remove `{requested}` from {}: the same declaration also binds `{}`, \
         and removing it would silently unbind {}. \
         Name every binding to remove the whole declaration: `resq rm decl {} {} {}`.",
        file.display(),
        missing.join("`, `"),
        if missing.len() == 1 { "that" } else { "those" },
        file.display(),
        requested,
        missing.join(" ")
    );
}

// =============================================================================================
// Content selection: exactly one of --content / stdin
// =============================================================================================

/// Read stdin when it is piped or redirected, `None` when it is an interactive terminal (reading
/// it would block) or carries only whitespace.
fn read_stdin_if_piped() -> Result<Option<String>> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Ok(None);
    }
    let mut buffer = String::new();
    stdin
        .read_to_string(&mut buffer)
        .context("failed to read declaration content from stdin")?;
    if buffer.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(buffer))
}

/// Content comes from `--content` **or** stdin — exactly one. Both is an error (we cannot know
/// which the user meant), neither is an error (there is nothing to write).
///
/// Split out from [`read_stdin_if_piped`] so the decision is a pure function the tests can drive;
/// `stdin` is `None` when stdin was a terminal or was empty.
pub fn choose_content(flag: Option<String>, stdin: Option<String>) -> Result<String> {
    match (flag, stdin) {
        (Some(_), Some(_)) => {
            bail!("resq set decl: --content and stdin both provided; pass exactly one")
        }
        (Some(content), None) => Ok(content),
        (None, Some(content)) => Ok(content),
        (None, None) => bail!(
            "resq set decl: no content; pass --content <SRC> or pipe the declaration on stdin"
        ),
    }
}

/// The names declared by a `set decl` content fragment.
///
/// The fragment must contain exactly one declaration. Decorators, doc comments and ordinary
/// comments are siblings rather than declarations (SPEC §1 finding 1), so
/// `"/** doc */ @genType let x = 1"` is still one declaration — but two `let`s are not, because
/// `set decl` upserts at a single dot-path.
fn content_declaration_names(content: &str) -> Result<Vec<String>> {
    let tree = parser::parse(content).context("failed to parse the given content")?;
    if tree.root_node().has_error() {
        let where_ = match parser::first_error_location(&tree, content) {
            Some((line, col)) => format!(" at {line}:{col}"),
            None => String::new(),
        };
        bail!("resq set decl: the given content does not parse{where_}");
    }

    let root = tree.root_node();
    let mut cursor = root.walk();
    let declared: Vec<Declaration> = root
        .named_children(&mut cursor)
        .filter_map(|child| parser::declaration_from_node(child, content, &ModulePath::root()))
        .collect();

    match declared.as_slice() {
        [] => bail!("resq set decl: the given content contains no declaration"),
        [one] if one.names.is_empty() => {
            bail!("resq set decl: the given content declares no name")
        }
        [one] => Ok(one.names.clone()),
        many => bail!(
            "resq set decl: the given content contains {} declarations; \
             it must contain exactly one",
            many.len()
        ),
    }
}

// =============================================================================================
// Tree outline: every addressable declaration, plus every module body we can append into
// =============================================================================================

/// A declaration paired with the node it came from — `Declaration` carries line numbers only, and
/// splicing needs byte offsets.
struct Located<'a> {
    decl: Declaration,
    node: Node<'a>,
}

struct Outline<'a> {
    decls: Vec<Located<'a>>,
    /// `(module path, body block)` for the file root and every `module X = { … }` with a body.
    /// `set decl --name Inner.helper` appends into the block registered under `Inner`.
    blocks: Vec<(ModulePath, Node<'a>)>,
}

impl<'a> Outline<'a> {
    fn of(root: Node<'a>, src: &str) -> Outline<'a> {
        let mut outline = Outline {
            decls: Vec::new(),
            blocks: vec![(ModulePath::root(), root)],
        };
        outline.walk(root, src, &ModulePath::root());
        outline
    }

    /// Only structural declarations are addressable (SPEC §3.1): top-level ones and members of
    /// `module … = { … }` blocks. We never descend into an expression body, so a `let` inside a
    /// function is correctly invisible to dot-path resolution — and therefore cannot be deleted by
    /// accident.
    fn walk(&mut self, node: Node<'a>, src: &str, path: &ModulePath) {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            let Some(decl) = parser::declaration_from_node(child, src, path) else {
                continue;
            };
            let is_module = decl.kind == DeclarationKind::Module;
            let name = decl.names.first().cloned();
            self.decls.push(Located { decl, node: child });

            if is_module && let (Some(name), Some(block)) = (name, parser::module_body_block(child))
            {
                let inner = path.child(name);
                self.blocks.push((inner.clone(), block));
                self.walk(block, src, &inner);
            }
        }
    }

    /// The single declaration at `path`. Zero matches is "not found", more than one is
    /// "ambiguous" — a write command never guesses which one the user meant.
    fn resolve(&self, file: &Path, path: &ModulePath, op: &str) -> Result<&Located<'a>> {
        let hits: Vec<&Located> = self.decls.iter().filter(|l| l.decl.is_at(path)).collect();
        match hits.as_slice() {
            [] => bail!(
                "resq {op}: no declaration at `{path}` in {}",
                file.display()
            ),
            [hit] => Ok(hit),
            many => bail!(
                "resq {op}: `{path}` is ambiguous in {} ({} matching declarations)",
                file.display(),
                many.len()
            ),
        }
    }

    fn block_at(&self, path: &ModulePath) -> Option<Node<'a>> {
        self.blocks
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, node)| *node)
    }
}

// =============================================================================================
// Splicing primitives
// =============================================================================================

fn splice(src: &str, start: usize, end: usize, text: &str) -> String {
    let mut out = String::with_capacity(src.len() + text.len());
    out.push_str(&src[..start]);
    out.push_str(text);
    out.push_str(&src[end..]);
    out
}

/// The leading whitespace of the line containing `offset`.
fn line_indent_at(src: &str, offset: usize) -> String {
    let line_start = src[..offset].rfind('\n').map_or(0, |i| i + 1);
    src[line_start..offset]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// Re-indent a content fragment for insertion at `indent`: the first line is left alone (the
/// insertion point already sits at the right column) and every later non-blank line is prefixed,
/// preserving the fragment's own relative indentation.
fn indent_continuation_lines(content: &str, indent: &str) -> String {
    if indent.is_empty() {
        return content.to_string();
    }
    let mut out = String::with_capacity(content.len() + indent.len() * 4);
    for (i, line) in content.lines().enumerate() {
        if i > 0 {
            out.push('\n');
            if !line.trim().is_empty() {
                out.push_str(indent);
            }
        }
        out.push_str(line);
    }
    out
}

/// Append a new declaration under `target`'s parent module, which must exist.
fn append_declaration(
    src: &str,
    outline: &Outline,
    file: &Path,
    target: &ModulePath,
    content: &str,
) -> Result<String> {
    let (parent, _) = target
        .split_leaf()
        .expect("target path is non-empty, so it splits");
    let Some(block) = outline.block_at(&parent) else {
        bail!(
            "resq set decl: cannot create `{target}` in {}: no module `{parent}` in this file",
            file.display()
        );
    };

    if parent.is_empty() {
        return Ok(append_at_file_end(src, content));
    }
    append_into_block(src, block, content)
}

fn append_at_file_end(src: &str, content: &str) -> String {
    let trimmed = src.trim_end();
    let mut out = String::with_capacity(src.len() + content.len() + 2);
    if !trimmed.is_empty() {
        out.push_str(trimmed);
        out.push_str("\n\n");
    }
    out.push_str(content);
    out.push('\n');
    out
}

/// Insert `content` as the last member of a module body, just before its closing `}`.
fn append_into_block(src: &str, block: Node, content: &str) -> Result<String> {
    let block_text = &src[block.start_byte()..block.end_byte()];
    let Some(rel_close) = block_text.rfind('}') else {
        bail!("resq set decl: module body has no closing brace; refusing to guess where to insert");
    };
    let close = block.start_byte() + rel_close;

    let mut cursor = block.walk();
    let last_member = block.named_children(&mut cursor).last();

    // Insert after the last member (or straight after `{` in an empty module), taking the
    // indentation from that member so the new declaration lines up with its siblings.
    let (insert_at, indent) = match last_member {
        Some(member) => {
            let (member_start, _) = parser::decl_span_with_attachments(member, src);
            (member.end_byte(), line_indent_at(src, member_start))
        }
        None => {
            let open = block_text.find('{').map_or(0, |i| i + 1) + block.start_byte();
            let mut indent = line_indent_at(src, block.start_byte());
            indent.push_str("  ");
            (open, indent)
        }
    };

    let mut inserted = String::from("\n\n");
    inserted.push_str(&indent);
    inserted.push_str(&indent_continuation_lines(content, &indent));
    // `module M = { let a = 1 }` keeps its closing brace on the same line; give it one so the
    // appended declaration is not swallowed into a trailing comment or run together with `}`.
    if !src[insert_at..close].contains('\n') {
        inserted.push('\n');
        inserted.push_str(&line_indent_at(src, block.start_byte()));
    }
    Ok(splice(src, insert_at, insert_at, &inserted))
}

/// Delete `start..end`, then tidy the hole: the removed declaration's own line indentation and
/// trailing newline go with it, and the join is collapsed to at most one blank line so `rm decl`
/// does not leave a widening gap behind.
///
/// Only the immediate join is normalized — blank lines elsewhere in the file are the author's and
/// are left exactly as they were.
fn remove_span(src: &str, start: usize, end: usize) -> String {
    let bytes = src.as_bytes();

    // Take the indentation that preceded the declaration, if that is all the line holds.
    let line_start = src[..start].rfind('\n').map_or(0, |i| i + 1);
    let start = if src[line_start..start]
        .chars()
        .all(|c| c == ' ' || c == '\t')
    {
        line_start
    } else {
        start
    };

    // ...and the trailing spaces plus the newline that terminated it.
    let mut end = end;
    while end < src.len() && matches!(bytes[end], b' ' | b'\t' | b'\r') {
        end += 1;
    }
    if end < src.len() && bytes[end] == b'\n' {
        end += 1;
    }

    let before = &src[..start];
    let after = &src[end..];

    // Nothing meaningful left after the hole: close the file with a single newline.
    if after.trim().is_empty() {
        let mut out = before.trim_end().to_string();
        if !out.is_empty() {
            out.push('\n');
        }
        return out;
    }
    // Nothing before it either: the declaration led the file, so drop the blank lines it left.
    if before.trim().is_empty() {
        return after.trim_start_matches('\n').to_string();
    }

    // First or last member of a module body: leave the brace flush against its neighbour rather
    // than opening (or closing) the block on a blank line.
    if before.trim_end().ends_with('{') || after.trim_start().starts_with('}') {
        let mut out = before.trim_end_matches('\n').to_string();
        out.push('\n');
        out.push_str(after.trim_start_matches('\n'));
        return out;
    }

    let trailing = before.len() - before.trim_end_matches('\n').len();
    let leading = after.len() - after.trim_start_matches('\n').len();
    let keep = if trailing + leading > 2 {
        2usize.saturating_sub(trailing)
    } else {
        leading
    };

    let mut out = String::with_capacity(src.len());
    out.push_str(before);
    for _ in 0..keep {
        out.push('\n');
    }
    out.push_str(after.trim_start_matches('\n'));
    out
}

/// Convenience for callers holding string paths (the CLI shape) rather than [`ModulePath`]s.
pub fn parse_paths(paths: &[String]) -> Vec<ModulePath> {
    paths.iter().map(|p| ModulePath::parse(p)).collect()
}
