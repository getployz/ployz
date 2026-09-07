use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=compose-helper");
    let target_os = env::var("CARGO_CFG_TARGET_OS").expect("Cargo provides the target OS");
    let target_arch =
        env::var("CARGO_CFG_TARGET_ARCH").expect("Cargo provides the target architecture");
    let os = match target_os.as_str() {
        "linux" => "linux",
        "macos" => "darwin",
        other => panic!("unsupported Compose helper OS: {other}"),
    };
    let arch = match target_arch.as_str() {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => panic!("unsupported Compose helper architecture: {other}"),
    };
    let output =
        PathBuf::from(env::var_os("OUT_DIR").expect("Cargo provides the build output directory"))
            .join("ployz-compose");
    let status = Command::new("go")
        .current_dir("compose-helper")
        .env("CGO_ENABLED", "0")
        .env("GOOS", os)
        .env("GOARCH", arch)
        .args([
            "build",
            "-mod=readonly",
            "-trimpath",
            "-ldflags=-s -w",
            "-o",
        ])
        .arg(output)
        .arg(".")
        .status()
        .expect("building Ployz requires Go for the bundled Compose helper");
    assert!(status.success(), "Compose helper build failed");
}
