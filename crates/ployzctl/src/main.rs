use std::process::ExitCode;

use ployzctl::commands::{parse_command, render_command};

fn main() -> ExitCode {
    match parse_command(std::env::args().skip(1)) {
        Ok(command) => {
            print!("{}", render_command(command));
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
