//! `resq refs` — project-wide reference resolution (SPEC §3.2, §3.4, §3.10).
//!
//! WAVE 3 (agent A7) owns this file.
//!
//! # Why this is NOT a port of elmq's `refs.rs`
//!
//! elmq narrows the search using each file's `import` list: a file that does not import `Foo`
//! cannot reference `Foo.bar`, so it is skipped outright. **ReScript has no such filter.** Every
//! module in a project is globally visible by basename, and `Foo.bar` requires no statement at all
//! (SPEC §3.2). So `refs` here scans *every* source file the project config points at, and has to
//! resolve visibility per file, per byte offset:
//!
//! | Form | What it adds |
//! |---|---|
//! | *(nothing)* | `Foo.bar` — always available |
//! | `open Foo` | `bar` referenceable **unqualified** |
//! | `module F = Foo` | adds `F.bar` |
//! | `include Foo` | splices `Foo`'s contents in (treated like `open` here) |
//!
//! # The honesty contract
//!
//! Under-reporting is the dangerous direction: a missed reference before a rename silently breaks
//! the build, and an empty result reads as "safe to rename". So every judgement call in this module
//! is biased towards **over-reporting, labelled**:
//!
//! * A bare identifier that matches the target name in a file where the target module is opened is
//!   reported even when a local binder shadows it — as [`RefKind::UnqualifiedShadowed`], so the
//!   caller can see it is probably not a real reference without us having silently dropped it.
//! * A later `open` that would really shadow an earlier one is not modelled; both stay reported.
//! * A partially-qualified path (`Inner.helper` under `open Main`) is matched against every `open`
//!   in scope, not just the one the compiler would pick.
//!
//! The one thing that is *not* over-reported is [SPEC §3.10]'s out-of-scope constructs: a
//! polymorphic-variant target is rejected with an explicit "unsupported" error rather than
//! returning zero results.
//!
//! # Known approximations (documented, not hidden)
//!
//! * **`include` is not transitive.** `include A` where `A` itself `include`s the target does not
//!   put the target's names in unqualified scope here. Likewise a re-export (`Main.msg` after
//!   `include Types` inside `Main`) is not resolved.
//! * **Module signatures are not read.** We never check that the opened module actually exports the
//!   name; matching is by name alone. That is an over-report, so it is safe.
//! * **Record fields are not references.** A field name in `{name: 1}` or in a `record_pattern`
//!   is skipped. Fields are not addressable declarations in resq (they never appear in
//!   [`crate::FileSummary::declarations`]), so nothing can target one, and treating them as value
//!   references is precisely the corruption SPEC §1 finding 8 warns about.
//! * **Decorator paths are opaque.** `@Foo.bar` parses as a single `decorator_identifier` leaf with
//!   no `module_identifier` child, so a module referenced only from a decorator is not found.
//! * **`Module.(expr)` / `Module.{…}` local opens do not parse** under the pinned grammar (see the
//!   report — this is an upstream gap beyond the three in SPEC §0.1). Files using them are still
//!   scanned, but the local open is invisible and the file is flagged as having parse errors.

use crate::cli::Format;
use crate::project::{Namespace, Project};
use crate::{DeclarationKind, ModulePath, analysis, parser, project};
use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use tree_sitter::Node;

// -------------------------------------------------------------------------------------------
// Public surface
// -------------------------------------------------------------------------------------------

/// How a reference reaches its target. Printed verbatim in both output formats, so an agent can
/// filter on it — in particular, [`RefKind::UnqualifiedShadowed`] marks the reports that are most
/// likely to be false positives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RefKind {
    /// The declaration (or module) itself, at its definition site. A `.resi` signature entry for
    /// the same declaration counts as a definition too — you must edit it during a rename.
    Definition,
    /// Fully qualified through the module's real name: `Types.msg`, `Ns.Types.msg`.
    Qualified,
    /// Qualified through a local `module F = Types` alias: `F.msg`.
    ViaAlias,
    /// Reachable only because of an `open`/`include` in scope — either a bare `msg`, or a
    /// partially-qualified `Inner.helper` under `open Main`.
    UnqualifiedViaOpen,
    /// Same as [`RefKind::UnqualifiedViaOpen`], but a local binder with the same name is visible at
    /// this offset, so the compiler probably resolves it to that binder instead. Reported rather
    /// than dropped — see the module docs' honesty contract.
    UnqualifiedShadowed,
}

impl fmt::Display for RefKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            RefKind::Definition => "definition",
            RefKind::Qualified => "qualified",
            RefKind::ViaAlias => "via-alias",
            RefKind::UnqualifiedViaOpen => "unqualified-via-open",
            RefKind::UnqualifiedShadowed => "unqualified-shadowed",
        })
    }
}

/// One reported reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub file: PathBuf,
    /// 1-indexed line.
    pub line: usize,
    /// 1-indexed column, counting characters not bytes (via [`parser::byte_offset_to_line_col`]).
    pub column: usize,
    /// Dot-path of the *enclosing* declaration, `None` for a reference outside every declaration
    /// (a top-level `open` line, say).
    pub path: Option<ModulePath>,
    pub kind: RefKind,
    /// The matched source text.
    pub text: String,
    /// The dotted target this reference resolves to, e.g. `Types.msg`.
    pub target: String,
    /// Byte offset of the match, used for stable ordering.
    pub byte: usize,
}

/// What the user asked about, resolved to an absolute module chain plus an optional member name.
///
/// `refs src/Types.res` -> `chain = ["Types"], name = None` (the module itself).
/// `refs src/Types.res msg` -> `chain = ["Types"], name = Some("msg")`.
/// `refs src/Main.res Inner.helper` -> `chain = ["Main", "Inner"], name = Some("helper")`.
/// `refs src/Main.res Inner` (a nested *module*) -> `chain = ["Main", "Inner"], name = None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub chain: Vec<String>,
    pub name: Option<String>,
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.chain.join("."))?;
        if let Some(name) = &self.name {
            write!(f, ".{name}")?;
        }
        Ok(())
    }
}

/// Handler for `Command::Refs`. Errors (no project root, unknown dot-path, unsupported target) come
/// back as `Err` and become a non-zero exit through `main`.
pub fn run(file: PathBuf, names: Vec<String>, format: Format) -> Result<()> {
    let refs = find(&file, &names)?;

    match format {
        Format::Compact => {
            let width = refs
                .iter()
                .map(|r| r.kind.to_string().len())
                .max()
                .unwrap_or(0);
            for r in &refs {
                let decl = r
                    .path
                    .as_ref()
                    .map(ModulePath::to_string)
                    .unwrap_or_else(|| "-".to_string());
                let kind = r.kind.to_string();
                println!(
                    "{}:{}:{}  {kind:width$}  {decl}  {}",
                    r.file.display(),
                    r.line,
                    r.column,
                    r.text
                );
            }
        }
        Format::Json => {
            for r in &refs {
                let value = serde_json::json!({
                    "file": r.file.display().to_string(),
                    "line": r.line,
                    "column": r.column,
                    "decl": r.path.as_ref().map(ModulePath::to_string),
                    "kind": r.kind,
                    "text": r.text,
                    "target": r.target,
                });
                println!("{}", serde_json::to_string(&value)?);
            }
        }
    }

    if refs.is_empty() {
        eprintln!(
            "note: no references found. The target dot-path resolved (an unknown one is an \
             error, and a polymorphic-variant one is refused as unsupported), and the definition \
             site is normally reported too — so an empty list here is unusual; re-check the \
             warnings above for files that could not be read or parsed."
        );
    }
    Ok(())
}

/// The pure seam: every reference to `names` (or to the module `file` defines, when `names` is
/// empty), across every source file the project config points at.
///
/// Read command (SPEC §2): a file that fails to parse warns on stderr and is still scanned for
/// whatever the parser recovered, rather than aborting the run.
pub fn find(file: &Path, names: &[String]) -> Result<Vec<Reference>> {
    // Discovery and config parsing are separate failures with very different fixes, so locate the
    // root first — otherwise a malformed `rescript.json` is reported as "no project root".
    project::find_root(file).with_context(|| {
        format!(
            "`resq refs` needs a ReScript project root to know which files to scan \
             (SPEC §3.2: there is no import list to narrow the search from {})",
            file.display()
        )
    })?;
    let project = Project::discover(file)?;

    let module_name = project::file_to_module_name(file);
    let targets = resolve_targets(file, names, &module_name)?;

    let namespace = match &project.config.namespace {
        Namespace::Named(n) => Some(n.clone()),
        Namespace::None => None,
    };

    // A4's note: `source_files()` re-walks the filesystem on every call. Walk once, reuse.
    let mut sources = project.source_files()?;
    let target_file = absolute(file);
    if !sources.iter().any(|s| absolute(s) == target_file) {
        eprintln!(
            "warning: {} is not under any `sources` entry in {}; scanning it anyway",
            file.display(),
            project.config_path.display()
        );
        sources.push(file.to_path_buf());
    }

    // `.res` and its sibling `.resi` both *define* the declaration — a rename must touch both, so
    // the signature entry is reported as a definition rather than missed.
    let mut defining: Vec<PathBuf> = vec![target_file.clone()];
    if let Some(sib) = project::find_sibling(file) {
        defining.push(absolute(&sib));
    }

    let mut out = Vec::new();
    for path in &sources {
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("warning: could not read {}: {e}", path.display());
                continue;
            }
        };
        let tree = match parser::parse(&source) {
            Ok(t) => t,
            Err(e) => {
                eprintln!(
                    "warning: could not parse {} ({e:#}); skipped",
                    path.display()
                );
                continue;
            }
        };
        if tree.root_node().has_error() {
            eprintln!(
                "warning: {} has parse errors; reference results for it are partial",
                path.display()
            );
        }

        let scan = FileScan::build(tree.root_node(), &source, namespace.as_deref());
        let is_defining = defining.contains(&absolute(path));
        let display = display_path(path);
        for target in &targets {
            scan.collect(target, &display, is_defining, &mut out);
        }
    }

    out.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.byte.cmp(&b.byte))
            .then(a.target.cmp(&b.target))
            .then(a.kind.to_string().cmp(&b.kind.to_string()))
    });
    out.dedup();
    Ok(out)
}

// -------------------------------------------------------------------------------------------
// Target resolution
// -------------------------------------------------------------------------------------------

fn resolve_targets(file: &Path, names: &[String], module_name: &str) -> Result<Vec<Target>> {
    if names.is_empty() {
        return Ok(vec![Target {
            chain: vec![module_name.to_string()],
            name: None,
        }]);
    }

    let source = std::fs::read_to_string(file)
        .with_context(|| format!("failed to read {}", file.display()))?;
    let tree = parser::parse(&source)?;
    let summary = analysis::extract_summary(&tree, &source, module_name);

    let mut targets = Vec::new();
    for raw in names {
        // SPEC §3.10: polymorphic variants are out of scope. Refuse loudly — returning zero
        // results here would read as "nothing references this", which is exactly the answer that
        // makes a rename destructive.
        if raw.split('.').any(|seg| seg.starts_with('#')) {
            bail!(
                "unsupported target `{raw}`: polymorphic variants are out of scope for v1 \
                 (SPEC §3.10). resq cannot resolve `#`-constructor references and will not \
                 report zero results for one — resolve this by hand before renaming."
            );
        }

        let path = ModulePath::parse(raw);
        if path.is_empty() {
            bail!("empty dot-path: `{raw}`");
        }
        let decl = summary.find_declaration(&path).with_context(|| {
            format!(
                "no declaration at dot-path `{raw}` in {} (SPEC §3.1: paths are exact — a bare \
                 name never matches a nested declaration)",
                file.display()
            )
        })?;

        if decl.kind == DeclarationKind::Type
            && has_polyvar_type(tree.root_node(), decl.start_line, decl.end_line)
        {
            eprintln!(
                "warning: `{raw}` is a polymorphic-variant type; references to its `#` \
                 constructors are unsupported (SPEC §3.10). Only references to the type name \
                 `{raw}` are reported below."
            );
        }

        let mut chain = vec![module_name.to_string()];
        let target = if decl.kind == DeclarationKind::Module {
            chain.extend(path.segments().iter().cloned());
            Target { chain, name: None }
        } else {
            let (parent, leaf) = path
                .split_leaf()
                .expect("a non-empty ModulePath always splits");
            chain.extend(parent.segments().iter().cloned());
            Target {
                chain,
                name: Some(leaf.to_string()),
            }
        };
        targets.push(target);
    }

    Ok(targets)
}

/// Whether any `polyvar_type` node falls inside a 1-indexed line range — used only to decide
/// whether to warn that a type's `#` constructors are out of scope.
fn has_polyvar_type(node: Node, start_line: usize, end_line: usize) -> bool {
    let row = node.start_position().row + 1;
    if node.kind() == "polyvar_type" && row >= start_line && row <= end_line {
        return true;
    }
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();
    children
        .into_iter()
        .any(|c| has_polyvar_type(c, start_line, end_line))
}

// -------------------------------------------------------------------------------------------
// Per-file scope model
// -------------------------------------------------------------------------------------------

/// A `module F = A.B` alias, valid from just after its declaration to the end of the block that
/// holds it.
struct Alias {
    name: String,
    chain: Vec<String>,
    from: usize,
    to: usize,
}

/// An `open A.B` / `include A.B`, with the same byte-range scope rule as [`Alias`].
struct Open {
    chain: Vec<String>,
    from: usize,
    to: usize,
}

/// One addressable declaration, for enclosing-path annotation and definition-site detection.
struct DeclEntry {
    /// The path of the module that *encloses* this declaration.
    enclosing: ModulePath,
    /// `enclosing` + the first bound name — what gets printed as the reference's context.
    primary: ModulePath,
    /// `(name, start_byte, end_byte)` for every name the declaration binds.
    names: Vec<(String, usize, usize)>,
    full_start: usize,
    full_end: usize,
}

/// One occurrence of an identifier-shaped thing, before it is matched against a target.
enum Occ<'a> {
    /// A module path: `Belt.Array`, the `Types` in `Types.msg`, `<Foo.Bar/>`.
    Mod { chain: Vec<String>, node: Node<'a> },
    /// A qualified member: `Types.msg`, `Arr.map`, `Types.Increment`.
    Qual {
        chain: Vec<String>,
        leaf: String,
        node: Node<'a>,
    },
    /// A bare identifier: `msg`, `Increment`, `helper`.
    Bare { name: String, node: Node<'a> },
}

impl Occ<'_> {
    fn node(&self) -> Node<'_> {
        match self {
            Occ::Mod { node, .. } | Occ::Qual { node, .. } | Occ::Bare { node, .. } => *node,
        }
    }
}

/// Everything derived from one parsed source file, computed once and reused for every target.
struct FileScan<'a> {
    src: &'a str,
    namespace: Option<&'a str>,
    aliases: Vec<Alias>,
    opens: Vec<Open>,
    /// Byte spans that *bind* a name rather than referencing one — pattern binders, parameters,
    /// declaration names, variant declarations, argument labels. Built on
    /// [`parser::bound_name_spans`] for everything pattern-shaped (SPEC §1 finding 8).
    binders: HashSet<(usize, usize)>,
    decls: Vec<DeclEntry>,
    /// `(path, block start, block end)` for every nested module body in the file.
    module_blocks: Vec<(ModulePath, usize, usize)>,
    occurrences: Vec<Occ<'a>>,
}

impl<'a> FileScan<'a> {
    fn build(root: Node<'a>, src: &'a str, namespace: Option<&'a str>) -> FileScan<'a> {
        let mut scan = FileScan {
            src,
            namespace,
            aliases: Vec::new(),
            opens: Vec::new(),
            binders: HashSet::new(),
            decls: Vec::new(),
            module_blocks: Vec::new(),
            occurrences: Vec::new(),
        };
        scan.collect_scopes(root);
        collect_binders(root, src, &mut scan.binders);
        collect_decls(root, src, &ModulePath::root(), &mut scan);
        collect_occurrences(root, src, &mut scan.occurrences);
        scan
    }

    /// `open`/`include`/alias statements, each scoped from its own end to the end of the block that
    /// contains it. That is the ReScript rule and it covers local `open`s inside a function body or
    /// a nested module as well as the file-level ones.
    fn collect_scopes(&mut self, node: Node<'a>) {
        let kind = node.kind();
        if matches!(kind, "open_statement" | "include_statement") {
            if let Some(first) = node.named_child(0) {
                let chain = module_chain(first, self.src);
                if !chain.is_empty() {
                    self.opens.push(Open {
                        chain,
                        from: node.end_byte(),
                        to: scope_end(node),
                    });
                }
            }
        } else if kind == "module_declaration"
            && let Some((name, target)) = parser::module_alias_parts(node, self.src)
        {
            self.aliases.push(Alias {
                name,
                chain: target.split('.').map(str::to_string).collect(),
                from: node.end_byte(),
                to: scope_end(node),
            });
        }

        let mut cursor = node.walk();
        let children: Vec<Node> = node.named_children(&mut cursor).collect();
        for child in children {
            self.collect_scopes(child);
        }
    }

    /// Expand aliases and strip the project namespace until the chain stops changing.
    ///
    /// The namespace matters because a namespaced project can spell the same module either way:
    /// `Types.msg` inside the package, `Proj.Types.msg` from a consumer. Both must match.
    fn resolve_chain(&self, chain: &[String], offset: usize) -> (Vec<String>, bool) {
        let mut out = chain.to_vec();
        let mut via_alias = false;
        for _ in 0..8 {
            if out.is_empty() {
                break;
            }
            if let Some(ns) = self.namespace
                && out.len() > 1
                && out[0] == ns
            {
                out.remove(0);
                continue;
            }
            // Innermost/last-declared alias wins, matching shadowing order.
            if let Some(alias) = self
                .aliases
                .iter()
                .rev()
                .find(|a| a.name == out[0] && offset >= a.from && offset < a.to)
            {
                let mut next = alias.chain.clone();
                next.extend_from_slice(&out[1..]);
                if next == out {
                    break;
                }
                out = next;
                via_alias = true;
                continue;
            }
            break;
        }
        (out, via_alias)
    }

    fn opens_in_scope(&self, offset: usize) -> Vec<Vec<String>> {
        self.opens
            .iter()
            .filter(|o| offset >= o.from && offset < o.to)
            .map(|o| self.resolve_chain(&o.chain, offset).0)
            .collect()
    }

    fn enclosing(&self, offset: usize) -> Option<&DeclEntry> {
        self.decls
            .iter()
            .filter(|d| d.full_start <= offset && offset < d.full_end)
            .min_by_key(|d| d.full_end - d.full_start)
    }

    /// Byte spans in this file that *define* the target — the declaration's own name(s), or a
    /// nested module's binding name. Only consulted for the defining `.res`/`.resi` pair.
    fn definition_spans(&self, target: &Target) -> HashSet<(usize, usize)> {
        let (enclosing, wanted) = match &target.name {
            Some(name) => (ModulePath(target.chain[1..].to_vec()), Some(name.as_str())),
            None if target.chain.len() > 1 => (
                ModulePath(target.chain[1..target.chain.len() - 1].to_vec()),
                target.chain.last().map(String::as_str),
            ),
            None => return HashSet::new(),
        };
        let Some(wanted) = wanted else {
            return HashSet::new();
        };

        self.decls
            .iter()
            .filter(|d| d.enclosing == enclosing)
            .flat_map(|d| d.names.iter())
            .filter(|(name, _, _)| name == wanted)
            .map(|&(_, s, e)| (s, e))
            .collect()
    }

    /// Whether the target module's members are referenceable *unqualified* at `offset`.
    fn unqualified_in_scope(&self, target: &Target, offset: usize, is_defining: bool) -> bool {
        if self.opens_in_scope(offset).contains(&target.chain) {
            return true;
        }
        if !is_defining {
            return false;
        }
        // Inside the file that defines the module, its own members need no qualification.
        if target.chain.len() == 1 {
            return true;
        }
        let inner = &target.chain[1..];
        self.module_blocks
            .iter()
            .any(|(path, s, e)| path.segments() == inner && offset >= *s && offset < *e)
    }

    fn collect(
        &self,
        target: &Target,
        display: &Path,
        is_defining: bool,
        out: &mut Vec<Reference>,
    ) {
        let definitions = if is_defining {
            self.definition_spans(target)
        } else {
            HashSet::new()
        };

        // A whole-file module target has no name node to point at — the *file* is the definition
        // (SPEC §3.1: `src/Foo.res` **is** module `Foo`). Reported at 1:1 with no enclosing
        // declaration, since it is the file itself and not anything inside it.
        if is_defining && target.name.is_none() && target.chain.len() == 1 {
            out.push(Reference {
                file: display.to_path_buf(),
                line: 1,
                column: 1,
                path: None,
                kind: RefKind::Definition,
                text: target.chain[0].clone(),
                target: target.to_string(),
                byte: 0,
            });
        }

        for occ in &self.occurrences {
            let node = occ.node();
            let span = (node.start_byte(), node.end_byte());

            let kind = match occ {
                Occ::Mod { chain, .. } => {
                    if target.name.is_some() {
                        continue;
                    }
                    // A `module Inner = { … }` binding name is a binder, not a reference — but in
                    // the defining file it is the target module's definition site.
                    if self.binders.contains(&span) {
                        if definitions.contains(&span) {
                            Some(RefKind::Definition)
                        } else {
                            continue;
                        }
                    } else {
                        self.match_module(chain, span.0, target)
                    }
                }
                Occ::Qual { chain, leaf, .. } => {
                    if target.name.as_deref() != Some(leaf.as_str()) {
                        continue;
                    }
                    self.match_member(chain, span.0, target)
                }
                Occ::Bare { name, .. } => {
                    if target.name.as_deref() != Some(name.as_str()) {
                        continue;
                    }
                    if self.binders.contains(&span) {
                        if definitions.contains(&span) {
                            Some(RefKind::Definition)
                        } else {
                            continue;
                        }
                    } else if self.unqualified_in_scope(target, span.0, is_defining) {
                        if is_shadowed(node, name, self.src) {
                            Some(RefKind::UnqualifiedShadowed)
                        } else {
                            Some(RefKind::UnqualifiedViaOpen)
                        }
                    } else {
                        continue;
                    }
                }
            };

            if let Some(kind) = kind {
                out.push(self.reference(display, span.0, span.1, kind, target));
            }
        }
    }

    /// Match a bare module-path occurrence against a *module* target. A prefix match counts:
    /// `Types.Nested.deep` does reference module `Types`.
    fn match_module(&self, chain: &[String], offset: usize, target: &Target) -> Option<RefKind> {
        let (resolved, via_alias) = self.resolve_chain(chain, offset);
        if resolved.starts_with(&target.chain) {
            return Some(if via_alias {
                RefKind::ViaAlias
            } else {
                RefKind::Qualified
            });
        }
        for open in self.opens_in_scope(offset) {
            let mut full = open;
            full.extend_from_slice(&resolved);
            if full.starts_with(&target.chain) {
                return Some(RefKind::UnqualifiedViaOpen);
            }
        }
        None
    }

    /// Match a qualified `chain.leaf` occurrence against a *member* target. The leaf has already
    /// been checked by the caller.
    fn match_member(&self, chain: &[String], offset: usize, target: &Target) -> Option<RefKind> {
        let (resolved, via_alias) = self.resolve_chain(chain, offset);
        if resolved == target.chain {
            return Some(if via_alias {
                RefKind::ViaAlias
            } else {
                RefKind::Qualified
            });
        }
        // `Inner.helper` under `open Main` is a reference to `Main.Inner.helper`.
        for open in self.opens_in_scope(offset) {
            let mut full = open;
            full.extend_from_slice(&resolved);
            if full == target.chain {
                return Some(RefKind::UnqualifiedViaOpen);
            }
        }
        None
    }

    fn reference(
        &self,
        display: &Path,
        start: usize,
        end: usize,
        kind: RefKind,
        target: &Target,
    ) -> Reference {
        let (line, column) = parser::byte_offset_to_line_col(self.src, start);
        Reference {
            file: display.to_path_buf(),
            line,
            column,
            path: self.enclosing(start).map(|d| d.primary.clone()),
            kind,
            text: self
                .src
                .get(start..end)
                .map(str::to_string)
                .unwrap_or_default(),
            target: target.to_string(),
            byte: start,
        }
    }
}

/// The end of the block (or file) an `open`/alias statement's effect runs to.
fn scope_end(node: Node) -> usize {
    node.parent().unwrap_or(node).end_byte()
}

// -------------------------------------------------------------------------------------------
// Occurrence collection
// -------------------------------------------------------------------------------------------

/// Node kinds whose contents are never code references. Polyvars are out of scope (SPEC §3.10)
/// and string/comment text is not code.
const OPAQUE_KINDS: &[&str] = &[
    "block_comment",
    "line_comment",
    "string",
    "polyvar",
    "polyvar_type",
    "polyvar_string",
    "polyvar_identifier",
    "polyvar_declaration",
];

/// The module segments a module-path-shaped node names, left to right.
///
/// `module_identifier_path` nests (`Belt.Array.map` puts `Belt.Array` in its own node), so this
/// recurses rather than reading direct children only.
fn module_chain(node: Node, src: &str) -> Vec<String> {
    match node.kind() {
        "module_identifier" | "jsx_identifier" => node_text(node, src)
            .into_iter()
            .filter(|t| !t.is_empty())
            .collect(),
        "module_identifier_path" | "nested_jsx_identifier" => {
            let mut out = Vec::new();
            let mut cursor = node.walk();
            let children: Vec<Node> = node.named_children(&mut cursor).collect();
            for child in children {
                out.extend(module_chain(child, src));
            }
            out
        }
        _ => Vec::new(),
    }
}

fn node_text(node: Node, src: &str) -> Option<String> {
    node.utf8_text(src.as_bytes()).ok().map(str::to_string)
}

fn collect_occurrences<'a>(node: Node<'a>, src: &str, out: &mut Vec<Occ<'a>>) {
    let kind = node.kind();
    if OPAQUE_KINDS.contains(&kind) {
        return;
    }

    match kind {
        // `Types.msg`, `Arr.map`, `Types.Increment` — first named child is the module part
        // (possibly itself a dotted path), last is the member.
        "value_identifier_path" | "type_identifier_path" | "nested_variant_identifier" => {
            push_qualified(node, src, out);
            return;
        }
        // `{Types.name: 1}` parses the field as a `property_identifier` *with children*. A plain
        // record field is a childless `property_identifier` and is not a reference at all.
        "property_identifier" => {
            if node.named_child_count() >= 2 {
                push_qualified(node, src, out);
            }
            return;
        }
        // SPEC §1 finding 8: `record_pattern` is flat and ambiguous — `{x: a}` gives two sibling
        // `value_identifier`s, the field name and the binder, and *neither* is a value reference.
        // Rather than re-deriving the field-vs-binder split (the one thing this module must not
        // do), skip every direct identifier child and recurse only into nested patterns.
        "record_pattern" => {
            let mut cursor = node.walk();
            let children: Vec<Node> = node.named_children(&mut cursor).collect();
            for child in children {
                if child.kind() != "value_identifier" {
                    collect_occurrences(child, src, out);
                }
            }
            return;
        }
        "module_identifier_path" | "module_identifier" => {
            let chain = module_chain(node, src);
            if !chain.is_empty() {
                out.push(Occ::Mod { chain, node });
            }
            return;
        }
        // `<Foo.Bar />` is module `Foo.Bar`, and the element itself references its `make`.
        "nested_jsx_identifier" | "jsx_identifier" => {
            let chain = module_chain(node, src);
            if chain
                .first()
                .is_some_and(|s| s.starts_with(char::is_uppercase))
            {
                out.push(Occ::Mod {
                    chain: chain.clone(),
                    node,
                });
                out.push(Occ::Qual {
                    chain,
                    leaf: "make".to_string(),
                    node,
                });
            }
            return;
        }
        "value_identifier" | "type_identifier" | "variant_identifier" => {
            if let Some(text) = node_text(node, src)
                && !text.is_empty()
                && text != "_"
                // `'a` in `type t<'a>` is a type variable, never a module member.
                && !text.starts_with('\'')
            {
                out.push(Occ::Bare { name: text, node });
            }
            return;
        }
        _ => {}
    }

    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();
    for child in children {
        collect_occurrences(child, src, out);
    }
}

/// Split a dotted path node into `(module chain, member)` and record both views: the chain is a
/// module reference in its own right, and the pair is a member reference.
fn push_qualified<'a>(node: Node<'a>, src: &str, out: &mut Vec<Occ<'a>>) {
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();
    if children.len() < 2 {
        return;
    }
    let chain = module_chain(children[0], src);
    if chain.is_empty() {
        return;
    }
    let leaf = node_text(children[children.len() - 1], src).unwrap_or_default();
    out.push(Occ::Mod {
        chain: chain.clone(),
        node,
    });
    if !leaf.is_empty() {
        out.push(Occ::Qual { chain, leaf, node });
    }
}

// -------------------------------------------------------------------------------------------
// Binder collection
// -------------------------------------------------------------------------------------------

/// Byte spans that introduce a name rather than reference one.
///
/// Everything pattern-shaped goes through [`parser::bound_name_spans`], the single implementation
/// of the `record_pattern` field-vs-binder disambiguation (SPEC §1 finding 8). Re-deriving it here
/// would risk treating record *field names* as variables.
///
/// `open`/`include` targets are deliberately **not** binders — `open Types` is a genuine reference
/// to module `Types` and must be reported as one.
fn collect_binders(node: Node, src: &str, out: &mut HashSet<(usize, usize)>) {
    for i in 0..node.child_count() as u32 {
        if matches!(
            node.field_name_for_child(i),
            Some("pattern") | Some("parameter")
        ) && let Some(child) = node.child(i)
        {
            out.extend(parser::bound_name_spans(child, src));
        }
    }

    match node.kind() {
        "parameter" | "labeled_parameter" => out.extend(parser::bound_name_spans(node, src)),
        // A functor parameter binds a *module* name, which `bound_name_spans` (values only) misses.
        "functor_parameter" => {
            if let Some(first) = node.named_child(0) {
                out.insert((first.start_byte(), first.end_byte()));
            }
        }
        // `| Increment` in `type msg = | Increment` declares the constructor.
        "variant_declaration" => {
            if let Some(first) = node.named_child(0)
                && first.kind() == "variant_identifier"
            {
                out.insert((first.start_byte(), first.end_byte()));
            }
        }
        "type_parameters" => {
            let mut cursor = node.walk();
            let children: Vec<Node> = node.named_children(&mut cursor).collect();
            for child in children {
                out.insert((child.start_byte(), child.end_byte()));
            }
        }
        // `make(~msg=1)`: `msg` is the argument *label*. A punned `~count` has a single child and
        // really is a reference, so only the multi-child form is suppressed.
        "labeled_argument" => {
            if node.named_child_count() > 1
                && let Some(first) = node.named_child(0)
            {
                out.insert((first.start_byte(), first.end_byte()));
            }
        }
        _ => {}
    }

    if parser::DECLARATION_KINDS.contains(&node.kind()) {
        out.extend(
            decl_name_spans(node, src)
                .into_iter()
                .map(|(_, s, e)| (s, e)),
        );
    }

    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();
    for child in children {
        collect_binders(child, src, out);
    }
}

/// `(name, start, end)` for every name a declaration node introduces. `open`/`include` introduce
/// nothing (their operand is a reference), so they yield an empty list.
fn decl_name_spans(node: Node, src: &str) -> Vec<(String, usize, usize)> {
    let named = |n: Node| node_text(n, src).map(|t| (t, n.start_byte(), n.end_byte()));
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();

    match node.kind() {
        "let_declaration" => {
            let mut out = Vec::new();
            for binding in children.iter().filter(|c| c.kind() == "let_binding") {
                for i in 0..binding.child_count() as u32 {
                    if binding.field_name_for_child(i) != Some("pattern") {
                        continue;
                    }
                    let Some(pattern) = binding.child(i) else {
                        continue;
                    };
                    for (s, e) in parser::bound_name_spans(pattern, src) {
                        if let Some(text) = src.get(s..e) {
                            out.push((text.to_string(), s, e));
                        }
                    }
                }
            }
            out
        }
        "type_declaration" => children
            .iter()
            .filter(|c| c.kind() == "type_binding")
            .filter_map(|b| b.child_by_field_name("name"))
            .filter_map(named)
            .collect(),
        "module_declaration" => children
            .iter()
            .filter(|c| c.kind() == "module_binding")
            .filter_map(|b| b.child_by_field_name("name"))
            .filter_map(named)
            .collect(),
        "external_declaration" => children
            .iter()
            .filter(|c| c.kind() == "value_identifier")
            .copied()
            .filter_map(named)
            .collect(),
        _ => Vec::new(),
    }
}

// -------------------------------------------------------------------------------------------
// Declaration ranges (enclosing dot-path) and nested module bodies
// -------------------------------------------------------------------------------------------

fn collect_decls(node: Node, src: &str, path: &ModulePath, scan: &mut FileScan) {
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();

    for child in children {
        let Some(kind) = parser::declaration_kind(child) else {
            continue;
        };
        let names = decl_name_spans(child, src);
        let (full_start, _) = parser::decl_span_with_attachments(child, src);
        let primary = match names.first() {
            Some((name, _, _)) => path.child(name.clone()),
            None => path.clone(),
        };

        scan.decls.push(DeclEntry {
            enclosing: path.clone(),
            primary,
            names,
            full_start,
            full_end: child.end_byte(),
        });

        if kind != DeclarationKind::Module {
            continue;
        }
        // One `module_declaration` can hold several `module_binding`s, each with its own body.
        let mut bcursor = child.walk();
        let bindings: Vec<Node> = child
            .named_children(&mut bcursor)
            .filter(|c| c.kind() == "module_binding")
            .collect();
        for binding in bindings {
            let Some(name) = binding
                .child_by_field_name("name")
                .and_then(|n| node_text(n, src))
            else {
                continue;
            };
            let Some(body) = module_binding_body(binding) else {
                continue;
            };
            let child_path = path.child(name);
            scan.module_blocks
                .push((child_path.clone(), body.start_byte(), body.end_byte()));
            collect_decls(body, src, &child_path, scan);
        }
    }
}

/// The block a `module_binding` defines — `block` for a body, the functor's body for a functor,
/// `None` for an alias (SPEC §1 finding 9).
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

// -------------------------------------------------------------------------------------------
// Shadowing
// -------------------------------------------------------------------------------------------

/// Whether a local binder with the same name is visible at `node`.
///
/// This is deliberately a *labelling* heuristic, not a filter: a hit it returns `true` for is still
/// reported, as [`RefKind::UnqualifiedShadowed`]. So a false negative here costs an unhelpfully
/// confident label, never a missed reference.
///
/// Covers the three cases that actually shadow an `open`ed name in practice:
/// enclosing function parameters, enclosing `switch` patterns, and an earlier declaration in an
/// enclosing block. A declaration that *contains* the occurrence is skipped — in `let msg = msg`
/// the right-hand `msg` is the outer one.
fn is_shadowed(node: Node, name: &str, src: &str) -> bool {
    let offset = node.start_byte();
    let mut current = node;
    while let Some(parent) = current.parent() {
        match parent.kind() {
            "function" => {
                for i in 0..parent.child_count() as u32 {
                    if !matches!(
                        parent.field_name_for_child(i),
                        Some("parameters") | Some("parameter")
                    ) {
                        continue;
                    }
                    if let Some(child) = parent.child(i)
                        && spans_bind(parser::bound_name_spans(child, src), name, src)
                    {
                        return true;
                    }
                }
            }
            "switch_match" => {
                for i in 0..parent.child_count() as u32 {
                    if parent.field_name_for_child(i) != Some("pattern") {
                        continue;
                    }
                    if let Some(child) = parent.child(i)
                        && spans_bind(parser::bound_name_spans(child, src), name, src)
                    {
                        return true;
                    }
                }
            }
            "block" | "source_file" => {
                let mut cursor = parent.walk();
                let siblings: Vec<Node> = parent.named_children(&mut cursor).collect();
                for sibling in siblings {
                    if sibling.end_byte() > offset {
                        continue;
                    }
                    if !parser::DECLARATION_KINDS.contains(&sibling.kind()) {
                        continue;
                    }
                    if decl_name_spans(sibling, src)
                        .iter()
                        .any(|(n, _, _)| n == name)
                    {
                        return true;
                    }
                }
            }
            _ => {}
        }
        current = parent;
    }
    false
}

fn spans_bind(spans: Vec<(usize, usize)>, name: &str, src: &str) -> bool {
    spans
        .into_iter()
        .any(|(s, e)| src.get(s..e).is_some_and(|t| t == name))
}

// -------------------------------------------------------------------------------------------
// Paths
// -------------------------------------------------------------------------------------------

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Report paths relative to the working directory when possible — absolute project-root paths make
/// the output needlessly wide and unstable across machines.
fn display_path(path: &Path) -> PathBuf {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| path.strip_prefix(&cwd).ok().map(Path::to_path_buf))
        .unwrap_or_else(|| path.to_path_buf())
}

// -------------------------------------------------------------------------------------------
// Internal unit tests — white-box checks on helpers. End-to-end coverage is in `tests/refs.rs`.
// -------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_chain_flattens_nested_paths() {
        let src = "let i = Belt.Array.map\n";
        let tree = parser::parse(src).unwrap();
        let mut occs = Vec::new();
        collect_occurrences(tree.root_node(), src, &mut occs);
        let chains: Vec<Vec<String>> = occs
            .iter()
            .filter_map(|o| match o {
                Occ::Mod { chain, .. } => Some(chain.clone()),
                _ => None,
            })
            .collect();
        assert!(chains.contains(&vec!["Belt".to_string(), "Array".to_string()]));
    }

    #[test]
    fn record_field_names_are_not_occurrences() {
        // SPEC §1 finding 8's sibling trap on the *expression* side: `{name: 1}`'s `name` is a
        // field, not a value reference.
        let src = "let v = {name: 1}\n";
        let tree = parser::parse(src).unwrap();
        let mut occs = Vec::new();
        collect_occurrences(tree.root_node(), src, &mut occs);
        assert!(
            !occs
                .iter()
                .any(|o| matches!(o, Occ::Bare { name, .. } if name == "name")),
            "a record field name must not be reported as an identifier reference"
        );
    }

    #[test]
    fn alias_resolution_expands_a_chain() {
        let src = "module Arr = Belt.Array\nlet b = Arr.map\n";
        let tree = parser::parse(src).unwrap();
        let scan = FileScan::build(tree.root_node(), src, None);
        let offset = src.find("Arr.map").unwrap();
        let (resolved, via_alias) = scan.resolve_chain(&["Arr".to_string()], offset);
        assert_eq!(resolved, vec!["Belt".to_string(), "Array".to_string()]);
        assert!(via_alias);
    }

    #[test]
    fn namespace_prefix_is_stripped() {
        let src = "let a = Proj.Types.msg\n";
        let tree = parser::parse(src).unwrap();
        let scan = FileScan::build(tree.root_node(), src, Some("Proj"));
        let (resolved, _) =
            scan.resolve_chain(&["Proj".to_string(), "Types".to_string()], src.len() - 1);
        assert_eq!(resolved, vec!["Types".to_string()]);
    }

    #[test]
    fn open_scope_is_limited_to_its_block() {
        let src = "let f = () => {\n  open Types\n  msg\n}\nlet g = msg\n";
        let tree = parser::parse(src).unwrap();
        let scan = FileScan::build(tree.root_node(), src, None);

        let inside = src.find("  msg").unwrap() + 2;
        let outside = src.rfind("msg").unwrap();
        assert!(
            scan.opens_in_scope(inside)
                .contains(&vec!["Types".to_string()])
        );
        assert!(
            !scan
                .opens_in_scope(outside)
                .contains(&vec!["Types".to_string()]),
            "a local `open` must not leak past its block"
        );
    }

    #[test]
    fn local_binder_shadows_an_opened_name() {
        let src = "open Types\nlet f = (msg) => msg\n";
        let tree = parser::parse(src).unwrap();
        let mut occs = Vec::new();
        collect_occurrences(tree.root_node(), src, &mut occs);
        let body = occs
            .iter()
            .find(|o| matches!(o, Occ::Bare { name, node } if name == "msg" && node.start_byte() > src.find("=>").unwrap()))
            .expect("the function body's `msg`");
        assert!(is_shadowed(body.node(), "msg", src));
    }
}
