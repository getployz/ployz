use std::process::ExitCode;

fn main() -> ExitCode {
    match ployz::handlers::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(ployz::handlers::Error::Exit(code)) => ExitCode::from(code),
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
