fn main() {
    let ebpf_dir = std::path::PathBuf::from("../ebpf");

    aya_build::build_ebpf([ebpf_dir.join("src/main.rs")])
        .expect("failed to build eBPF program");

    println!("cargo:rerun-if-changed=../ebpf/src/main.rs");
    println!("cargo:rerun-if-changed=../ebpf-common/src/lib.rs");
}
