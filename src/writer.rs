//! Write path. Implements steps 2 and 3 of the write-safety invariant (SPEC §2); step 1 lives in
//! [`crate::parser::ensure_clean_parse`].
//!
//! The contract every mutating command owes the user:
//!
//! 1. `parser::ensure_clean_parse(&source, file)?` — refuse a file that is already broken.
//! 2. build the new buffer, then `writer::validate_output(&buffer, file, op)?` — refuse a buffer
//!    that would not parse, leaving the file byte-for-byte unchanged.
//! 3. `writer::atomic_write(file, &buffer)?`.
//!
//! [`validated_write`] does 2 and 3 together and is what commands should normally call. Reaching
//! for `atomic_write` directly means opting out of the invariant, so do it only when the buffer
//! has already been validated.
//!
//! Multi-file commands validate each file independently and may leave a partial write behind
//! (SPEC §2) — there is deliberately no cross-file staging layer.

use crate::parser;
use anyhow::{Context, Result, bail};
use std::path::Path;

/// Write `contents` to `path` without ever exposing a truncated file: write a sibling temp file in
/// the same directory, then rename over the target. Same-directory matters — `rename` is only
/// atomic within a filesystem.
pub fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let dir = dir.unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "out".to_string());
    let temp = dir.join(format!(".resq-tmp-{}-{}", std::process::id(), name));

    std::fs::write(&temp, contents)
        .with_context(|| format!("failed to write temp file: {}", temp.display()))?;
    if let Err(e) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(e).with_context(|| format!("failed to replace {}", path.display()));
    }
    Ok(())
}

/// Step 2 of the write-safety invariant (SPEC §2): re-parse the buffer resq is *about* to write
/// and refuse it if tree-sitter reports an ERROR or MISSING node.
///
/// This catches bugs in our own splicing as well as bad user input, so it runs on every mutation
/// even when the edit "obviously" cannot break the file. The message names the file, the attempted
/// operation, and the first error's `line:col` **in the would-be output** — the coordinate is in
/// the new buffer, not the on-disk file, which is what a caller needs to debug a rejected splice.
///
/// The on-disk file is untouched: nothing has been written at this point.
pub fn validate_output(buffer: &str, file: &Path, op: &str) -> Result<()> {
    let tree = parser::parse(buffer)
        .with_context(|| format!("failed to re-parse the new buffer for {}", file.display()))?;
    if tree.root_node().has_error() {
        let where_ = match parser::first_error_location(&tree, buffer) {
            Some((line, col)) => format!(" at {line}:{col}"),
            None => String::new(),
        };
        bail!(
            "refusing to write {}: '{op}' would produce a file that does not parse{where_}; \
             the file is unchanged",
            file.display()
        );
    }
    Ok(())
}

/// [`validate_output`] then [`atomic_write`] — steps 2 and 3 in one call. Prefer this over calling
/// `atomic_write` directly so a command cannot forget to validate.
pub fn validated_write(path: &Path, contents: &str, op: &str) -> Result<()> {
    validate_output(contents, path, op)?;
    atomic_write(path, contents)
}
