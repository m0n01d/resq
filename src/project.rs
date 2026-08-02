//! Project model: `rescript.json`/`bsconfig.json` discovery, config parsing, and source-file
//! walking (SPEC §3.4).
//!
//! This is the ReScript analogue of elmq's `elm.json` handling. There is no user-facing command
//! for it in this wave — `refs` (agent A7) is blocked on it, so the public API here is the
//! contract that module is written against. Keep it small; every `pub` item below is load-bearing
//! for a different agent who cannot ask questions.
//!
//! **Do not hardcode `src/`.** Real ReScript projects configure `sources` as a bare string, an
//! object with `dir`/`subdirs`, or an array of either — all three are handled by [`ProjectConfig::parse`].

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use walkdir::{DirEntry, WalkDir};

/// Filename of the ReScript v11+ project config. Preferred over [`BSCONFIG_JSON`] when both exist
/// in the same directory (SPEC §3.4).
pub const RESCRIPT_JSON: &str = "rescript.json";

/// Filename of the ReScript v10 (BuckleScript-era) project config.
pub const BSCONFIG_JSON: &str = "bsconfig.json";

/// A discovered ReScript project: its root directory plus its parsed config.
///
/// Obtain one with [`Project::discover`]. Commands that need project scope (`refs`, and in v2
/// `mv`/`rename decl`/`move-decl`) should call this once and reuse it rather than re-walking the
/// filesystem per file.
#[derive(Debug, Clone)]
pub struct Project {
    /// The directory containing the config file — the project root, and the base every entry in
    /// `sources` is relative to.
    pub root: PathBuf,
    /// Path to whichever config file was actually found (`rescript.json` or `bsconfig.json`).
    pub config_path: PathBuf,
    /// The parsed, normalized configuration.
    pub config: ProjectConfig,
}

impl Project {
    /// Discover the project that owns `start` (a file or a directory).
    ///
    /// Walks upward from `start` — resolved against the current working directory first if it is
    /// relative — until an ancestor containing `rescript.json` or `bsconfig.json` is found.
    /// Returns a clear error, not a panic, if no ancestor has one; callers like `refs` should
    /// surface that error directly rather than falling back to a guessed root.
    pub fn discover(start: &Path) -> Result<Project> {
        let root = find_root(start)?;
        let config_path = config_file_in(&root)
            .expect("find_root only returns directories that contain a config file");
        let text = std::fs::read_to_string(&config_path)
            .with_context(|| format!("failed to read {}", config_path.display()))?;
        let config = ProjectConfig::parse(&text)
            .with_context(|| format!("failed to parse {}", config_path.display()))?;
        Ok(Project {
            root,
            config_path,
            config,
        })
    }

    /// Every `.res`/`.resi` file under this project's configured `sources`, honoring each entry's
    /// `subdirs` flag. Skips `node_modules` and `lib` directories wherever encountered, since
    /// those hold dependency and compiler-output trees, never hand-written source.
    ///
    /// Order is deterministic (sorted, deduplicated) but otherwise unspecified; callers that care
    /// about source order should sort by their own criteria.
    pub fn source_files(&self) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        for source in &self.config.sources {
            files.extend(source.walk(&self.root)?);
        }
        files.sort();
        files.dedup();
        Ok(files)
    }
}

/// Walk upward from `start` looking for the nearest ancestor directory containing
/// `rescript.json` or `bsconfig.json`. `start` itself is checked first (after resolving to a
/// directory, if it names a file).
///
/// This is the primitive [`Project::discover`] is built on; exposed separately because some
/// callers (tests, `refs` error messages) want the bare root path without forcing a config parse.
pub fn find_root(start: &Path) -> Result<PathBuf> {
    let absolute = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to read the current directory")?
            .join(start)
    };

    let mut dir = if absolute.is_file() {
        match absolute.parent() {
            Some(parent) => parent.to_path_buf(),
            None => absolute.clone(),
        }
    } else {
        absolute.clone()
    };

    loop {
        if config_file_in(&dir).is_some() {
            return Ok(dir);
        }
        dir = match dir.parent() {
            Some(parent) => parent.to_path_buf(),
            None => bail!(
                "no {RESCRIPT_JSON} or {BSCONFIG_JSON} found in any ancestor of {}; \
                 this command requires a ReScript project root",
                start.display()
            ),
        };
    }
}

/// If `dir` directly contains a project config, its path — preferring `rescript.json`.
fn config_file_in(dir: &Path) -> Option<PathBuf> {
    let rescript_json = dir.join(RESCRIPT_JSON);
    if rescript_json.is_file() {
        return Some(rescript_json);
    }
    let bsconfig_json = dir.join(BSCONFIG_JSON);
    if bsconfig_json.is_file() {
        return Some(bsconfig_json);
    }
    None
}

/// How a project's modules are namespaced (`rescript.json`/`bsconfig.json` `"namespace"` field).
///
/// `namespace: true` derives the namespace from the config's `"name"` field (dashes/underscores
/// split into words, each capitalized, concatenated — e.g. `"my-app"` -> `"MyApp"`); a string
/// value is used verbatim; absent or `false` means no namespacing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Namespace {
    #[default]
    None,
    Named(String),
}

/// One `sources` entry after normalizing all three shapes real configs use: a bare string, an
/// object, or (via [`ProjectConfig::parse`] flattening an array) one element of an array of
/// either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDir {
    /// Directory path, relative to the project root (e.g. `"src"`).
    pub dir: String,
    /// Whether to recurse into subdirectories. `false` for a bare string entry or an object that
    /// omits `subdirs`.
    pub subdirs: bool,
}

impl SourceDir {
    /// `.res`/`.resi` files directly under this source dir (and, if `subdirs`, everywhere below
    /// it), skipping `node_modules`/`lib`. `root` is the project root this entry's `dir` is
    /// relative to. A source directory that does not exist on disk yields no files rather than an
    /// error — a stale `sources` entry shouldn't break every other command.
    fn walk(&self, root: &Path) -> Result<Vec<PathBuf>> {
        let base = root.join(&self.dir);
        if !base.is_dir() {
            return Ok(Vec::new());
        }

        let mut files = Vec::new();
        if self.subdirs {
            for entry in WalkDir::new(&base)
                .into_iter()
                .filter_entry(|e| !is_excluded_dir(e))
            {
                let entry =
                    entry.with_context(|| format!("walking {}", base.display()))?;
                if entry.file_type().is_file() && is_source_file(entry.path()) {
                    files.push(entry.path().to_path_buf());
                }
            }
        } else {
            let read_dir = std::fs::read_dir(&base)
                .with_context(|| format!("reading directory {}", base.display()))?;
            for entry in read_dir {
                let entry = entry
                    .with_context(|| format!("reading directory {}", base.display()))?;
                let path = entry.path();
                if entry.file_type()?.is_file() && is_source_file(&path) {
                    files.push(path);
                }
            }
        }
        Ok(files)
    }
}

fn is_excluded_dir(entry: &DirEntry) -> bool {
    entry.file_type().is_dir()
        && entry
            .file_name()
            .to_str()
            .is_some_and(|name| name == "node_modules" || name == "lib")
}

fn is_source_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("res") | Some("resi")
    )
}

/// The fields of `rescript.json`/`bsconfig.json` that matter to resq (SPEC §3.4). Everything else
/// in the file (`package-specs`, `bs-dependencies`, `warnings`, ...) is ignored.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectConfig {
    /// Every configured source directory, in the order the config lists them. Polymorphic in the
    /// source JSON (string / object / array of either) but normalized here.
    pub sources: Vec<SourceDir>,
    /// How modules in this project are namespaced.
    pub namespace: Namespace,
    /// The compiled-output suffix (e.g. `.res.mjs`, `.bs.js`), verbatim from the config. `None`
    /// when the config omits it — resq does not guess the compiler's default, since it has
    /// changed across ReScript versions and isn't needed to walk `.res`/`.resi` sources.
    pub suffix: Option<String>,
}

impl ProjectConfig {
    /// Parse a `rescript.json`/`bsconfig.json` document's text.
    ///
    /// Public (rather than file-only) so callers — and this module's own tests — can exercise the
    /// three `sources` shapes inline without fixture files.
    pub fn parse(text: &str) -> Result<ProjectConfig> {
        let raw: RawConfig =
            serde_json::from_str(text).context("invalid JSON in project config")?;
        raw.try_into()
    }
}

/// Direct serde mirror of the on-disk JSON shape, before shape-normalization. Kept private:
/// callers only ever see the normalized [`ProjectConfig`].
#[derive(Debug, Deserialize)]
struct RawConfig {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    sources: Option<RawSources>,
    #[serde(default)]
    namespace: Option<RawNamespace>,
    #[serde(default)]
    suffix: Option<String>,
}

/// `sources` is a string, an object, or an array of either (SPEC task). `#[serde(untagged)]` tries
/// each variant in order, so `One` (a bare string or a bare object) is tried before `Many`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawSources {
    One(RawSourceEntry),
    Many(Vec<RawSourceEntry>),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawSourceEntry {
    Dir(String),
    Detailed {
        dir: String,
        #[serde(default)]
        subdirs: bool,
    },
}

impl From<RawSourceEntry> for SourceDir {
    fn from(raw: RawSourceEntry) -> Self {
        match raw {
            RawSourceEntry::Dir(dir) => SourceDir {
                dir,
                subdirs: false,
            },
            RawSourceEntry::Detailed { dir, subdirs } => SourceDir { dir, subdirs },
        }
    }
}

/// `namespace` is a bool or a string.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawNamespace {
    Enabled(bool),
    Named(String),
}

impl TryFrom<RawConfig> for ProjectConfig {
    type Error = anyhow::Error;

    fn try_from(raw: RawConfig) -> Result<Self> {
        let raw_sources = raw
            .sources
            .context("project config has no `sources` field")?;
        let entries = match raw_sources {
            RawSources::One(entry) => vec![entry],
            RawSources::Many(entries) => entries,
        };
        let sources = entries.into_iter().map(SourceDir::from).collect();

        let namespace = match raw.namespace {
            None | Some(RawNamespace::Enabled(false)) => Namespace::None,
            Some(RawNamespace::Enabled(true)) => {
                Namespace::Named(namespace_from_package_name(raw.name.as_deref().unwrap_or("")))
            }
            Some(RawNamespace::Named(name)) => Namespace::Named(name),
        };

        Ok(ProjectConfig {
            sources,
            namespace,
            suffix: raw.suffix,
        })
    }
}

/// Derive an auto-namespace from a package `"name"` the way the ReScript build system does:
/// split on non-alphanumeric separators (`-`, `_`), capitalize each word, concatenate. `"my-app"`
/// -> `"MyApp"`, `"proj"` -> `"Proj"`.
fn namespace_from_package_name(name: &str) -> String {
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// The ReScript module name a source path defines: the file basename, capitalized, **regardless
/// of the directory it lives in** (SPEC §3.2). `src/nested/Util.res` is module `Util`, not
/// `Nested.Util` — ReScript modules are visible project-wide by basename alone, which is exactly
/// what makes [`refs`](crate) hard (there is no import list to narrow the search).
///
/// Delegates to [`crate::module_name_from_path`] (A1's implementation of the same rule) so there
/// is exactly one place this logic lives; re-exported here because A7 is written against
/// `project::file_to_module_name`, not `lib::module_name_from_path`.
pub fn file_to_module_name(path: &Path) -> String {
    crate::module_name_from_path(path)
}

/// Given a `.res` or `.resi` path, the path of its sibling (the other extension) — **without**
/// checking whether that file exists. `None` if `path` is neither a `.res` nor a `.resi` path.
pub fn sibling_path(path: &Path) -> Option<PathBuf> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("res") => Some(path.with_extension("resi")),
        Some("resi") => Some(path.with_extension("res")),
        _ => None,
    }
}

/// Given a `.res` or `.resi` path, its sibling's path **if that file exists on disk** — e.g. the
/// `.resi` for a `.res` that has one, for the `.resi` sync guard (SPEC §3.3). `None` both when
/// `path` isn't a `.res`/`.resi` path and when no sibling is present.
pub fn find_sibling(path: &Path) -> Option<PathBuf> {
    sibling_path(path).filter(|sib| sib.is_file())
}
