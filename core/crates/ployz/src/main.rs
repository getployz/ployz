#![deny(clippy::print_stdout)]

use std::process::ExitCode;

fn main() -> ExitCode {
    ployz::ui::mask_panics();
    clap_complete::CompleteEnv::with_factory(ployz::cli::command)
        .var(ployz::env::COMPLETE)
        .complete();
    ployz::ui::exit(ployz::handlers::run())
}
