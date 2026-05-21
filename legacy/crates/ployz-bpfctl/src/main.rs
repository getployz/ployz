use std::process::ExitCode;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() -> ExitCode {
    eprintln!("ployz-bpfctl is only supported on Linux");
    ExitCode::FAILURE
}
