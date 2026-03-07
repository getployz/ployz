#[cfg(target_os = "linux")]
fn main() {
    aya_build::build_ebpf(
        [aya_build::Package {
            name: "ployz-ebpf",
            root_dir: "ebpf",
            ..Default::default()
        }],
        aya_build::Toolchain::Nightly,
    )
    .expect("failed to build eBPF program");
}

#[cfg(not(target_os = "linux"))]
fn main() {
    // eBPF is Linux-only; nothing to do on other platforms
}
