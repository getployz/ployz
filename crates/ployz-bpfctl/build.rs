fn main() {
    aya_build::build_ebpf(
        [aya_build::Package {
            name: "ployz-ebpf",
            root_dir: "../../ebpf",
            ..Default::default()
        }],
        aya_build::Toolchain::Nightly,
    )
    .expect("failed to build eBPF program");
}
