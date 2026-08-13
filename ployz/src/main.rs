use std::process::ExitCode;

fn main() -> ExitCode {
    match ployz::handlers::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
