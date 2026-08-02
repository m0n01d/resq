//! `resq get` — extract the full source of one or more declarations by dot-path (SPEC §4, §3.1).
//!
//! This module deliberately does **not** depend on `analysis.rs` (A2's `FileSummary` builder,
//! built concurrently). It walks the parse tree itself using the spine helpers in `parser.rs`:
//! `parser::declaration_from_node` to read one declaration, `parser::module_body_block` to
//! recurse into nested modules, and — the one that matters most —
//! `parser::decl_span_with_attachments` so decorators and the doc comment travel with the
//! extracted source (SPEC §3.5). A `get` of `make` in a `@react.component` file that omits the
//! decorator returns code that does not compile; that is the failure this module exists to
//! prevent.
//!
//! `get` is a READ command (SPEC §2): tolerant of parse errors. It warns on stderr and extracts
//! whatever it can, rather than calling `ensure_clean_parse` and refusing outright.

use crate::cli::Format;
use crate::parser;
use crate::{Declaration, DeclarationKind, ModulePath};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use tree_sitter::Node;

/// One resolved `get` result, matching the `--format json` schema in the task spec:
/// `{file, path, kind, source, start_line, end_line, decorators, doc_comment}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GetResult {
    pub file: String,
    pub path: String,
    pub kind: DeclarationKind,
    pub source: String,
    pub start_line: usize,
    pub end_line: usize,
    pub decorators: Vec<String>,
    pub doc_comment: Option<String>,
}

/// A declaration paired with the tree node it was read from, so we can later recover its exact
/// source span (`Declaration` itself only carries line numbers, not byte offsets — see
/// `decl_span_with_attachments`, which needs the node).
struct Found<'a> {
    decl: Declaration,
    node: Node<'a>,
}

/// Walk `node`'s named children collecting every addressable declaration, recursing into module
/// bodies with the module's name appended to `path`. Mirrors what `analysis.rs`'s `FileSummary`
/// walk almost certainly does, but built independently per the wave-2 isolation rule — this module
/// must not import `analysis`.
///
/// Only structural declarations are addressable (SPEC §3.1): top-level declarations and members of
/// `module … = { … }` blocks. We never descend into a `let` binding's own expression body, so a
/// `let` nested inside a function is correctly invisible to dot-path resolution.
fn collect<'a>(node: Node<'a>, src: &str, path: &ModulePath, out: &mut Vec<Found<'a>>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let Some(decl) = parser::declaration_from_node(child, src, path) else {
            continue;
        };
        let is_module = decl.kind == DeclarationKind::Module;
        let primary_name = decl.names.first().cloned();
        out.push(Found { decl, node: child });

        if is_module
            && let (Some(name), Some(block)) = (primary_name, parser::module_body_block(child))
        {
            collect(block, src, &path.child(name), out);
        }
    }
}

/// Resolve `path` against the declarations found in `src`, and — on success — carve out its exact
/// source span including decorators and doc comment.
///
/// A bare name only matches a top-level declaration; a nested one requires a fully-qualified path
/// (SPEC §3.1: `Declaration::is_at` already enforces this, we don't re-derive it). Zero matches is
/// "not found"; more than one is "ambiguous" — both are errors, never a guess.
fn resolve_one(file: &Path, src: &str, found: &[Found], path: &ModulePath) -> Result<GetResult> {
    let hits: Vec<&Found> = found.iter().filter(|f| f.decl.is_at(path)).collect();
    match hits.as_slice() {
        [] => bail!("resq get: no declaration at `{path}` in {}", file.display()),
        [hit] => {
            let (start_byte, _) = parser::decl_span_with_attachments(hit.node, src);
            let source = src[start_byte..hit.node.end_byte()].to_string();
            Ok(GetResult {
                file: file.display().to_string(),
                path: path.to_string(),
                kind: hit.decl.kind,
                source,
                start_line: hit.decl.start_line,
                end_line: hit.decl.end_line,
                decorators: hit.decl.decorators.clone(),
                doc_comment: hit.decl.doc_comment.clone(),
            })
        }
        many => bail!(
            "resq get: `{path}` is ambiguous in {} ({} matching declarations)",
            file.display(),
            many.len()
        ),
    }
}

/// Extract every requested `paths` from one `file`, in requested order.
///
/// Reads and parses the file exactly once regardless of how many paths are requested against it.
/// Per SPEC §2, `get` is a tolerant read command: a parse error produces a stderr warning, not an
/// abort — extraction proceeds against whatever the grammar could recover.
pub fn extract_group(file: &Path, paths: &[String]) -> Result<Vec<GetResult>> {
    let src = fs::read_to_string(file)
        .with_context(|| format!("failed to read {}", file.display()))?;
    let tree = parser::parse(&src)
        .with_context(|| format!("failed to parse {}", file.display()))?;
    if tree.root_node().has_error() {
        eprintln!(
            "warning: {} has parse errors; extraction may be incomplete",
            file.display()
        );
    }

    let mut found = Vec::new();
    collect(tree.root_node(), &src, &ModulePath::root(), &mut found);

    paths
        .iter()
        .map(|p| resolve_one(file, &src, &found, &ModulePath::parse(p)))
        .collect()
}

/// Extract every group of `(file, paths)`, in order, concatenating results.
pub fn extract_many(groups: &[(PathBuf, Vec<String>)]) -> Result<Vec<GetResult>> {
    let mut all = Vec::new();
    for (file, paths) in groups {
        all.extend(extract_group(file, paths)?);
    }
    Ok(all)
}

/// A token from the flattened `-f` value list looks like a file, not a dot-path, if it carries a
/// path separator or one of ReScript's source suffixes. See [`group_from`] for why this matters.
fn looks_like_file(token: &str) -> bool {
    token.ends_with(".res")
        || token.ends_with(".resi")
        || token.contains('/')
        || token.contains('\\')
}

/// Regroup the flattened `-f` value list back into `(file, paths)` groups.
///
/// `cli::Command::Get.from` is declared as a single `Vec<String>` with
/// `action = ArgAction::Append` and `num_args = 2..` per occurrence (see `src/cli.rs`, which this
/// module may not edit). clap's derive extracts a repeated, variable-arity `Append` arg into one
/// flat `Vec<String>` via `get_many` — the boundary between successive `-f FILE PATH...`
/// occurrences is not preserved in the typed field. `-f a.res foo bar -f b.res baz` and
/// `-f a.res foo -f bar.res baz` are therefore indistinguishable from the flat vector alone
/// *unless* we can tell file tokens from path tokens on sight.
///
/// We can: every fixture and realistic invocation names a `.res`/`.resi` file (or a path
/// containing a directory separator), and a dot-path segment is a plain identifier — it is never
/// a bare `.res`/`.resi` suffix. So a token that looks like a file starts a new group; anything
/// else is a path belonging to the most recent group. This is a real ambiguity in the CLI shape as
/// declared, not a guess.
fn group_from(flat: &[String]) -> Result<Vec<(PathBuf, Vec<String>)>> {
    let mut groups: Vec<(PathBuf, Vec<String>)> = Vec::new();
    for token in flat {
        if looks_like_file(token) {
            groups.push((PathBuf::from(token), Vec::new()));
        } else {
            match groups.last_mut() {
                Some((_, paths)) => paths.push(token.clone()),
                None => bail!(
                    "resq get -f: expected a file (ending in `.res`/`.resi`) before `{token}`"
                ),
            }
        }
    }
    for (file, paths) in &groups {
        if paths.is_empty() {
            bail!("resq get -f {}: no dot-paths given", file.display());
        }
    }
    Ok(groups)
}

/// Reconcile the two CLI forms (SPEC §4) into a uniform list of `(file, paths)` groups:
///
/// * bare positional — `resq get <FILE> <PATH>...` — `file`/`names` populated, `from` empty.
/// * grouped — `resq get -f <FILE> <PATH>... [-f <FILE> <PATH>...]` — `from` populated, `file`
///   and `names` empty.
///
/// Mixing the two forms in one invocation is rejected rather than guessed at.
fn build_groups(
    file: Option<PathBuf>,
    names: Vec<String>,
    from: Vec<String>,
) -> Result<Vec<(PathBuf, Vec<String>)>> {
    match (file, from.is_empty()) {
        (Some(file), true) => {
            if names.is_empty() {
                bail!("resq get {}: at least one dot-path is required", file.display());
            }
            Ok(vec![(file, names)])
        }
        (None, false) => {
            if !names.is_empty() {
                bail!("resq get: cannot mix bare positional paths with -f groups");
            }
            group_from(&from)
        }
        (Some(_), false) => bail!("resq get: cannot mix bare <FILE> with -f groups"),
        (None, true) => bail!("resq get: requires either <FILE> <PATH>... or -f <FILE> <PATH>..."),
    }
}

/// Render results per `--format`. Compact prints raw declaration source (attachments included),
/// one per requested path, separated by a blank line. JSON emits an array of [`GetResult`].
fn render(results: &[GetResult], format: Format) {
    match format {
        Format::Compact => {
            let bodies: Vec<&str> = results.iter().map(|r| r.source.as_str()).collect();
            println!("{}", bodies.join("\n\n"));
        }
        Format::Json => {
            let json = serde_json::to_string_pretty(results).expect("GetResult is serializable");
            println!("{json}");
        }
    }
}

/// Entry point wired from `main.rs`'s `Command::Get` arm.
pub fn run(file: Option<PathBuf>, names: Vec<String>, from: Vec<String>, format: Format) -> Result<()> {
    let groups = build_groups(file, names, from)?;
    let results = extract_many(&groups)?;
    render(&results, format);
    Ok(())
}
