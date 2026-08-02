//! CONDUCTOR-OWNED. Subagents MUST NOT edit this file.
//!
//! Every v1 subcommand is pre-declared here so that parallel agents never contend on a shared
//! manifest (see WAVES.md §6). Agents implement their handler inside their own module and wire it
//! by replacing exactly one `unimplemented!()` line in `main.rs`.

use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "resq", about = "Query and edit ReScript files — like jq for ReScript")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Show a summary of one or more ReScript files
    List {
        #[arg(num_args = 1.., required = true)]
        files: Vec<PathBuf>,
        #[arg(long, value_enum, default_value_t = Format::Compact)]
        format: Format,
        #[arg(long)]
        docs: bool,
    },
    /// Extract the full source of one or more declarations by dot-path
    ///
    ///   resq get <FILE> <PATH>...            bare positional (single file)
    ///   resq get -f <FILE> <PATH>... [-f …]  grouped multi-file
    Get {
        file: Option<PathBuf>,
        #[arg(num_args = 0..)]
        names: Vec<String>,
        #[arg(short = 'f', long = "file", num_args = 2.., action = clap::ArgAction::Append)]
        from: Vec<String>,
        #[arg(long, value_enum, default_value_t = Format::Compact)]
        format: Format,
    },
    /// Search for a regex in ReScript sources, annotated with enclosing declaration
    Grep {
        pattern: String,
        path: Option<PathBuf>,
        #[arg(short = 'F', long)]
        fixed: bool,
        #[arg(short = 'i', long)]
        ignore_case: bool,
        #[arg(long)]
        include_comments: bool,
        #[arg(long)]
        include_strings: bool,
        #[arg(long)]
        definitions: bool,
        #[arg(long)]
        source: bool,
        #[arg(long, value_enum, default_value_t = Format::Compact)]
        format: Format,
    },
    /// Find all references to a module or declaration. Requires a project root.
    Refs {
        file: PathBuf,
        #[arg(num_args = 0..)]
        names: Vec<String>,
        #[arg(long, value_enum, default_value_t = Format::Compact)]
        format: Format,
    },
    /// Upsert a top-level or nested declaration
    Set {
        #[command(subcommand)]
        command: SetCommand,
    },
    /// Surgical find-and-replace within a declaration's scope
    Patch {
        file: PathBuf,
        name: String,
        #[arg(long)]
        old: String,
        #[arg(long)]
        new: String,
    },
    /// Remove declarations or open statements
    Rm {
        #[command(subcommand)]
        command: RmCommand,
    },
    /// Add an open statement or a module alias
    Add {
        #[command(subcommand)]
        command: AddCommand,
    },
    /// Add items to the sibling .resi interface file
    Expose {
        file: PathBuf,
        #[arg(num_args = 1.., required = true)]
        items: Vec<String>,
    },
    /// Remove items from the sibling .resi interface file
    Unexpose {
        file: PathBuf,
        #[arg(num_args = 1.., required = true)]
        items: Vec<String>,
    },
    /// Print the agent integration guide
    Guide,
}

#[derive(Subcommand)]
pub enum SetCommand {
    /// Upsert a declaration (content via --content or stdin)
    Decl(SetDecl),
}

#[derive(Args)]
pub struct SetDecl {
    pub file: PathBuf,
    /// Dot-path. Must match the parsed name in content if content has one.
    #[arg(long)]
    pub name: Option<String>,
    /// Inline content (exactly-one-of with stdin)
    #[arg(long)]
    pub content: Option<String>,
}

#[derive(Subcommand)]
pub enum RmCommand {
    Decl(RmDecl),
    Open(RmOpen),
}

#[derive(Args)]
pub struct RmDecl {
    pub file: PathBuf,
    #[arg(num_args = 1.., required = true)]
    pub names: Vec<String>,
}

#[derive(Args)]
pub struct RmOpen {
    pub file: PathBuf,
    #[arg(num_args = 1.., required = true)]
    pub modules: Vec<String>,
    /// Skip the unqualified-reference safety scan (see SPEC §3.2)
    #[arg(long)]
    pub force: bool,
}

#[derive(Subcommand)]
pub enum AddCommand {
    Open(AddOpen),
    Alias(AddAlias),
}

#[derive(Args)]
pub struct AddOpen {
    pub file: PathBuf,
    #[arg(num_args = 1.., required = true)]
    pub modules: Vec<String>,
}

#[derive(Args)]
pub struct AddAlias {
    pub file: PathBuf,
    /// `<Name>=<Module>`, e.g. `Arr=Belt.Array`
    #[arg(num_args = 1.., required = true)]
    pub aliases: Vec<String>,
}

#[derive(Clone, ValueEnum)]
pub enum Format {
    Compact,
    Json,
}
