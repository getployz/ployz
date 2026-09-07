use std::process::ExitCode;

fn main() -> ExitCode {
    sigpipe::reset();
    ployz::failure::terminate(ployz::handlers::run())
}
