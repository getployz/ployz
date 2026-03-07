# macOS Deploy Setup

Cross-compile ployz for Linux and deploy to remote servers from your Mac.

## Prerequisites

### 1. Rust + Linux cross-compilation target

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add x86_64-unknown-linux-gnu
```

Install a cross-linker (pick one):

```sh
# Option A: cargo-zigbuild (recommended)
brew install zig
cargo install cargo-zigbuild

# Option B: cross (uses Docker)
cargo install cross
```

### 2. Deploy targets

Create `.env.targets` in the repo root:

```sh
TARGETS="root@your-server-ip"
SSH_PORT=22
```

## Deploy

```sh
just deploy
```

This downloads pre-built eBPF bytecode from GitHub releases, cross-compiles with `--features ebpf-native`, uploads binaries, and restarts `ployzd`.

## How eBPF bytecode works

The BPF TC classifier is pre-built in CI and published as a GitHub release (`ebpf-v*` tags). The version is tracked in `.ebpf-version`.

`just deploy` automatically runs `just install-ebpf` which downloads the bytecode to `ebpf/target/bpfel-unknown-none/release/ployz-ebpf-tc`. The `build.rs` picks it up via `include_bytes!` — no nightly toolchain or bpf-linker needed on your Mac.

### Updating eBPF bytecode

1. Make changes to `ebpf/` or `ebpf-common/`
2. Push a tag: `git tag ebpf-v0.2.0 && git push --tags`
3. CI builds and releases the new bytecode
4. Update `.ebpf-version` to `v0.2.0`
5. `just deploy` picks up the new version

### Building eBPF locally (optional)

If you need to build the BPF bytecode locally (e.g. for iteration), you need nightly + bpf-linker. On Linux this works out of the box. On macOS:

```sh
rustup install nightly
rustup component add rust-src --toolchain nightly
brew install llvm
PATH="/opt/homebrew/opt/llvm/bin:$PATH" \
LLVM_SYS_200_PREFIX=/opt/homebrew/opt/llvm \
cargo +nightly install --no-default-features --features llvm-20 bpf-linker
```

Then build directly:

```sh
cd ebpf
cargo +nightly build -Z build-std=core --release --target bpfel-unknown-none
```

> Match the `llvm-XX` feature to your Homebrew LLVM version and the `LLVM_SYS_XX0_PREFIX` env var accordingly.
