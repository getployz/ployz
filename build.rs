#[cfg(target_os = "linux")]
fn main() {
    let ebpf_dir = std::path::PathBuf::from("ebpf");
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());

    aya_build::build_ebpf([ebpf_dir.join("src/main.rs")])
        .expect("failed to build eBPF program");

    // aya_build puts artifacts in OUT_DIR automatically
    println!("cargo:rerun-if-changed=ebpf/src/main.rs");
    println!("cargo:rerun-if-changed=ebpf-common/src/lib.rs");
}

#[cfg(not(target_os = "linux"))]
fn main() {
    // eBPF is Linux-only; nothing to do on other platforms
}
