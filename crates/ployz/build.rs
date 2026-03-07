fn main() {
    #[cfg(feature = "ebpf-native")]
    {
        let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
        let dst = std::path::PathBuf::from(&out_dir).join("ployz-ebpf-tc");

        // Use pre-built bytecode if available (downloaded from CI release)
        let prebuilt =
            std::path::Path::new("../ebpf/target/bpfel-unknown-none/release/ployz-ebpf-tc");
        if prebuilt.exists() {
            println!(
                "cargo:rerun-if-changed=../ebpf/target/bpfel-unknown-none/release/ployz-ebpf-tc"
            );
            std::fs::copy(prebuilt, &dst).expect("failed to copy pre-built eBPF bytecode");
            return;
        }

        // Fall back to building with aya-build (requires nightly + bpf-linker)
        aya_build::build_ebpf(
            [aya_build::Package {
                name: "ployz-ebpf",
                root_dir: "../ebpf",
                ..Default::default()
            }],
            aya_build::Toolchain::Nightly,
        )
        .expect("failed to build eBPF program");
    }
}
