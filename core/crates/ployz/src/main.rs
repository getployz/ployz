#![deny(clippy::print_stdout)]

use std::{io::IsTerminal, process::ExitCode};

fn main() -> ExitCode {
    clap_complete::CompleteEnv::with_factory(ployz::cli::command)
        .var(ployz::cli::env::COMPLETE)
        .complete();
    // Only a piped reader should be able to end the process silently. Interactive
    // runs keep Rust's SIGPIPE ignore so a daemon socket hang-up (for example the
    // restart after `initialize`) surfaces as an error instead of a silent exit.
    // ponytail: captured-stdout runs still die silently on a socket hang-up;
    // route stdout through a BrokenPipe-aware writer if that ever matters.
    if !std::io::stdout().is_terminal() {
        sigpipe::reset();
    }
    ployz::failure::terminate(ployz::handlers::run())
}
