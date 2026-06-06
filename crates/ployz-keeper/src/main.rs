use std::process::ExitCode;

use ployz_keeper::cli::{KeeperCliError, load_startup};

fn main() -> ExitCode {
    match load_startup(std::env::args_os().skip(1)) {
        Ok(startup) => {
            if startup.join_token.is_some() {
                println!("ployz-keeper started with bootstrap join material");
            } else {
                println!("ployz-keeper started");
            }
            ExitCode::SUCCESS
        }
        Err(KeeperCliError::HelpRequested) => {
            println!("{}", KeeperCliError::HelpRequested);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
