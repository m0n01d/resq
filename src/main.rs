//! CONDUCTOR-OWNED dispatch table. Subagents replace exactly one `unimplemented!()` line each.
use clap::Parser;
use resq::cli::{AddCommand, Cli, Command, RmCommand, SetCommand};

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::List { .. } => unimplemented!("A2: analysis.rs"),
        Command::Get { .. } => unimplemented!("A3: extract.rs"),
        Command::Grep { .. } => unimplemented!("A5: grep.rs"),
        Command::Refs { .. } => unimplemented!("A7: refs.rs"),
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
