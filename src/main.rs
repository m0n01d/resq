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
            SetCommand::Decl(_) => unimplemented!("A9: set decl"),
        },
        Command::Patch { .. } => unimplemented!("A9: patch"),
        Command::Rm { command } => match command {
            RmCommand::Decl(_) => unimplemented!("A9: rm decl"),
            RmCommand::Open(_) => unimplemented!("A8: rm open"),
        },
        Command::Add { command } => match command {
            AddCommand::Open(_) => unimplemented!("A8: add open"),
            AddCommand::Alias(_) => unimplemented!("A8: add alias"),
        },
        Command::Guide => unimplemented!("conductor: guide.md"),
    }
}
