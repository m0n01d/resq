//! Shared type spine. WAVE 1 (agent A1) owns this file.
//!
//! Everything downstream (`analysis`, `extract`, `grep`, `refs`, `imports`, `edit`) imports these
//! types. Two divergences from `elmq`'s `Declaration` are deliberate and load-bearing:
//!
//! 1. **`names: Vec<String>`, not `name: String`** — one ReScript binding can bind many names
//!    (`let (a, b) = pair`, `let {x, y} = record`, `let a = 1 and b = 2`). See SPEC §3.7.
//! 2. **`decorators: Vec<String>`** is new — ReScript decorators (`@react.component`, `@genType`)
//!    have no Elm analogue and MUST travel with the declaration on `get`/`set`/`rm`. See SPEC §3.5.
//!    Note they are *siblings* of the declaration node, not children (SPEC §1 finding 1); use
//!    [`parser::decl_span_with_attachments`] rather than rediscovering this.

pub mod cli;
pub mod extract;
pub mod parser;
pub mod writer;

use serde::{Serialize, Serializer};
use std::fmt;
use std::path::Path;

/// A dot-separated path addressing something inside a file (SPEC §3.1).
///
/// ReScript files may nest modules arbitrarily, so every declaration is addressed relative to the
/// file root: `helper`, `Inner.helper`, `Inner.Deep.helper`. A bare name never matches a nested
/// declaration — there is no implicit search.
///
/// The empty path is the file root itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub struct ModulePath(pub Vec<String>);

impl ModulePath {
    /// The file root — an empty path.
    pub fn root() -> Self {
        ModulePath(Vec::new())
    }

    /// Split a user-supplied dot-path such as `"Inner.Deep.helper"`.
    ///
    /// Empty input yields the root path. Empty segments (from `"A..b"`) are dropped rather than
    /// producing a segment that can never match.
    pub fn parse(s: &str) -> Self {
        ModulePath(
            s.split('.')
                .filter(|seg| !seg.is_empty())
                .map(str::to_string)
                .collect(),
        )
    }

    pub fn segments(&self) -> &[String] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// This path extended by one segment. Used when descending into a nested module.
    pub fn child(&self, segment: impl Into<String>) -> Self {
        let mut next = self.0.clone();
        next.push(segment.into());
        ModulePath(next)
    }

    /// Split into `(enclosing path, leaf segment)`. `None` for the root path.
    pub fn split_leaf(&self) -> Option<(ModulePath, &str)> {
        let (leaf, parent) = self.0.split_last()?;
        Some((ModulePath(parent.to_vec()), leaf.as_str()))
    }

    /// The last segment, i.e. the declaration name in an address like `Inner.helper`.
    pub fn leaf(&self) -> Option<&str> {
        self.0.last().map(String::as_str)
    }
}

impl fmt::Display for ModulePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.join("."))
    }
}

impl std::str::FromStr for ModulePath {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(ModulePath::parse(s))
    }
}

/// Serialized as the dotted string (`"Inner.Deep"`), not as an array — JSON consumers address
/// declarations with the same syntax the CLI accepts.
impl Serialize for ModulePath {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

/// Whether a `let` binding binds exactly one name through a plain identifier, or destructures.
///
/// SPEC §3.7: `let (a, b) = pair` and `let {x, y} = record` bind several names from one binding,
/// which changes what `rm`/`rename` are allowed to do. `let a = 1 and b = 2` binds two names but
/// through two *simple* binders, so it stays [`BinderKind::Simple`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BinderKind {
    /// Every pattern in the declaration is a plain identifier.
    Simple,
    /// At least one pattern is a tuple / record / array / list / as-pattern.
    Destructuring,
}

impl fmt::Display for BinderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BinderKind::Simple => write!(f, "simple"),
            BinderKind::Destructuring => write!(f, "destructuring"),
        }
    }
}

/// SPEC §3.9. `External` replaces elmq's `Port`; `Module`, `Include` and `Open` are new.
///
/// ReScript does not distinguish `type` from `type alias` at the syntax level the way Elm does —
/// one `type_declaration` covers both, so there is no `TypeAlias` variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclarationKind {
    Let,
    Type,
    Module,
    External,
    Include,
    Open,
}

impl fmt::Display for DeclarationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            DeclarationKind::Let => "let",
            DeclarationKind::Type => "type",
            DeclarationKind::Module => "module",
            DeclarationKind::External => "external",
            DeclarationKind::Include => "include",
            DeclarationKind::Open => "open",
        };
        f.write_str(s)
    }
}

/// One declaration, addressed by `path` + one of `names`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Declaration {
    /// Every name this declaration binds, in source order (SPEC §3.7). Never empty for a
    /// well-formed declaration; `open`/`include` carry the referenced module path as their single
    /// "name".
    pub names: Vec<String>,

    /// Path of the **enclosing module**, relative to the file root. This does **not** include the
    /// declaration's own name — a destructuring binding has several names and therefore several
    /// addresses. Use [`Declaration::full_paths`] to get the addressable dot-paths.
    ///
    /// A top-level declaration has `ModulePath::root()`; `helper` inside `module Inner` has
    /// `ModulePath(["Inner"])`.
    pub path: ModulePath,

    pub kind: DeclarationKind,
    pub binder_kind: BinderKind,

    /// Decorator source text in source order, e.g. `["@genType", "@react.component"]`.
    /// SPEC §3.5 — these MUST travel with the declaration on `get`/`set`/`rm`/`move-decl`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub decorators: Vec<String>,

    /// The annotated type, with the leading `:` stripped. `None` when un-annotated (SPEC §3.6).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_annotation: Option<String>,

    /// Raw doc-comment text including the `/**` and `*/` delimiters (SPEC §1 finding 2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_comment: Option<String>,

    /// 1-indexed first line of the declaration **including** its decorators and doc comment, so
    /// that `lines[start_line-1 ..= end_line-1]` is a self-contained, compilable slice.
    pub start_line: usize,

    /// 1-indexed last line of the declaration.
    pub end_line: usize,
}

impl Declaration {
    /// Every dot-path this declaration answers to: `path` extended by each bound name.
    pub fn full_paths(&self) -> Vec<ModulePath> {
        self.names.iter().map(|n| self.path.child(n)).collect()
    }

    /// The canonical dot-path — `path` + the first bound name.
    pub fn primary_path(&self) -> ModulePath {
        match self.names.first() {
            Some(name) => self.path.child(name),
            None => self.path.clone(),
        }
    }

    /// Whether this declaration is addressed by `path`.
    pub fn is_at(&self, path: &ModulePath) -> bool {
        match path.split_leaf() {
            Some((parent, leaf)) => self.path == parent && self.names.iter().any(|n| n == leaf),
            None => false,
        }
    }
}

/// A `module A = B.C` alias (SPEC §3.2). Distinct from a `module A = { … }` definition, which is a
/// [`DeclarationKind::Module`] declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModuleAlias {
    /// The local name (`Arr` in `module Arr = Belt.Array`).
    pub name: String,
    /// The aliased module path (`Belt.Array`).
    pub target: String,
}

/// The elmq `FileSummary` analogue.
///
/// ReScript has no module header line and no import list (SPEC §3.1, §3.2), so elmq's
/// `module_line` becomes `module_name` (derived from the file name — the file *is* the module) and
/// `imports` splits into `opens` + `aliases`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileSummary {
    /// The module this file defines — its basename with the first letter capitalized.
    pub module_name: String,

    /// Module paths brought into unqualified scope, in source order: `["Belt", "Js.Console"]`.
    pub opens: Vec<String>,

    /// `module X = Y.Z` aliases, in source order.
    pub aliases: Vec<ModuleAlias>,

    /// **Flat**, source-ordered list of every declaration in the file, nested ones included.
    /// Nesting is carried by [`Declaration::path`], not by structure; a `module` declaration
    /// precedes its own members. `list` renders the tree by re-deriving depth from `path.len()`.
    pub declarations: Vec<Declaration>,
}

impl FileSummary {
    pub fn new(module_name: impl Into<String>) -> Self {
        FileSummary {
            module_name: module_name.into(),
            opens: Vec::new(),
            aliases: Vec::new(),
            declarations: Vec::new(),
        }
    }

    /// Look up a declaration by its full dot-path (`"helper"`, `"Inner.Deep.helper"`).
    ///
    /// Matches a destructuring binding on *any* of the names it binds, so
    /// `find_declaration("second")` finds `let (first, second) = …`.
    pub fn find_declaration(&self, path: &ModulePath) -> Option<&Declaration> {
        self.declarations.iter().find(|d| d.is_at(path))
    }

    /// Every declaration at `path`. There should only ever be one in a compiling file, but `set`
    /// and `rm` want to notice shadowing rather than silently pick the first.
    pub fn find_declarations(&self, path: &ModulePath) -> Vec<&Declaration> {
        self.declarations.iter().filter(|d| d.is_at(path)).collect()
    }
}

/// The ReScript module name a source path defines: the basename, minus `.res`/`.resi`, with the
/// first character upper-cased (SPEC §3.1). Namespacing from `rescript.json` is A4's job.
pub fn module_name_from_path(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut chars = stem.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => stem,
    }
}
