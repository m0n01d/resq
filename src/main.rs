//! CONDUCTOR-OWNED dispatch table. Subagents replace exactly one `unimplemented!()` line each.
use clap::Parser;
use resq::cli::{AddCommand, Cli, Command, RmCommand, SetCommand};

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::List {
            files,
            format,
            docs,
        } => resq::analysis::run_list(files, format, docs),
        Command::Get {
            file,
            names,
            from,
            format,
        } => resq::extract::run(file, names, from, format),
        Command::Grep {
            pattern,
            path,
            fixed,
            ignore_case,
            include_comments,
            include_strings,
            definitions,
            source,
            format,
        } => std::process::exit(resq::grep::execute(resq::grep::GrepArgs {
            pattern,
            path,
            fixed,
            ignore_case,
            include_comments,
            include_strings,
            definitions,
            source,
            format,
        })),
        Command::Refs {
            file,
            names,
            format,
        } => resq::refs::run(file, names, format),
        Command::Set { command } => match command {
            SetCommand::Decl(args) => resq::edit::run_set_decl(args),
        },
        Command::Patch { file, name, old, new } => resq::edit::run_patch(&file, &name, &old, &new),
        Command::Rm { command } => match command {
            RmCommand::Decl(args) => resq::edit::run_rm_decl(args),
            RmCommand::Open(_) => unimplemented!("A8: rm open"),
            RmCommand::Decl(_) => unimplemented!("A9: rm decl"),
            RmCommand::Open(args) => resq::imports::run_rm_open(args),
        },
        Command::Add { command } => match command {
            AddCommand::Open(args) => resq::imports::run_add_open(args),
            AddCommand::Alias(args) => resq::imports::run_add_alias(args),
        },
        Command::Guide => unimplemented!("conductor: guide.md"),
    }
}
