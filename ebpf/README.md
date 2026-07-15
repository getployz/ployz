# eBPF subsystem

The Ployz eBPF subsystem has three components:

- `common` provides the shared map types and bytecode validation in the
  `ployz-ebpf-common` package.
- `control` builds the host-side `ployz-ebpf-ctl` control binary.
- `program` builds the `ployz-ebpf-tc` bytecode loaded by the control binary.

`common` and `control` are members of the repository workspace. `program` is
an independent Cargo workspace because it targets `bpfel-unknown-none` with a
nightly toolchain and `bpf-linker`. Build it through
`scripts/build-ebpf-bytecode.sh`; its output remains `ployz-ebpf-tc`.
