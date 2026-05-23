use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let ebpf_dir = manifest_dir.join("../../ebpf");
    println!("cargo:rerun-if-changed={}", ebpf_dir.display());

    let target_os = env::var("CARGO_CFG_TARGET_OS").expect("CARGO_CFG_TARGET_OS");
    if target_os != "linux" {
        return;
    }

    let target_endian = env::var("CARGO_CFG_TARGET_ENDIAN").expect("CARGO_CFG_TARGET_ENDIAN");
    let ebpf_target = match target_endian.as_str() {
        "little" => "bpfel-unknown-none",
        "big" => "bpfeb-unknown-none",
        other => panic!("unsupported endian: {other}"),
    };

    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").expect("CARGO_CFG_TARGET_ARCH");
    let bpf_target_arch = match target_arch.as_str() {
        arch if arch.starts_with("riscv64") => "riscv64",
        arch => arch,
    };

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let ebpf_target_dir = out_dir.join("ployz-ebpf");
    let manifest_path = ebpf_dir.join("Cargo.toml");
    let rustflags = format!(
        "--cfg=bpf_target_arch=\"{bpf_target_arch}\"\u{1f}-Cdebuginfo=2\u{1f}-Clink-arg=--btf"
    );

    let status = Command::new("rustup")
        .args([
            "run",
            "nightly",
            "cargo",
            "build",
            "--manifest-path",
            manifest_path
                .to_str()
                .expect("manifest path is valid UTF-8"),
            "--package",
            "ployz-ebpf",
            "-Z",
            "build-std=core",
            "--bins",
            "--release",
            "--target",
            ebpf_target,
            "--target-dir",
            ebpf_target_dir
                .to_str()
                .expect("target dir path is valid UTF-8"),
        ])
        .env("CARGO_ENCODED_RUSTFLAGS", rustflags)
        .env_remove("RUSTC")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .status()
        .expect("failed to invoke cargo for eBPF build");

    assert!(status.success(), "failed to build eBPF program");

    let src = ebpf_target_dir
        .join(ebpf_target)
        .join("release")
        .join("ployz-ebpf-tc");
    let dst = out_dir.join("ployz-ebpf-tc");
    copy_binary(&src, &dst);
}

fn copy_binary(src: &Path, dst: &Path) {
    std::fs::copy(src, dst).unwrap_or_else(|error| {
        panic!(
            "failed to copy eBPF binary from {} to {}: {error}",
            src.display(),
            dst.display()
        )
    });
}
