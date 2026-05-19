use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> mvp_runtime::RuntimeResult<()> {
    let mut args = std::env::args().skip(1);
    let mut addr = None;
    let mut root = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "runtime-http" => {}
            "--addr" => addr = args.next(),
            "--root" => root = args.next().map(PathBuf::from),
            _ => {}
        }
    }
    let addr = addr.unwrap_or_else(|| "127.0.0.1:0".to_string());
    let root = root.unwrap_or_else(|| PathBuf::from("."));
    mvp_runtime::run_static_http_server(&addr, &root)
}
