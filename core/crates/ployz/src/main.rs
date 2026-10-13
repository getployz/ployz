#![deny(clippy::print_stdout)]

use std::process::ExitCode;

fn main() -> ExitCode {
    clap_complete::CompleteEnv::with_factory(ployz::cli::command)
        .var(ployz::cli::env::COMPLETE)
        .complete();
    ployz::ui::exit(ployz::handlers::run())
}
