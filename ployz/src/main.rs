use std::process::ExitCode;

fn main() -> ExitCode {
    ployz::failure::terminate(ployz::handlers::run())
}
