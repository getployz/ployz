# macOS Deploy Setup

Cross-compile ployz for Linux and deploy to remote servers from your Mac.

## Prerequisites

### 1. Rust (stable + nightly)

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup install nightly
rustup component add rust-src --toolchain nightly
```

### 2. Linux cross-compilation target

```sh
rustup target add x86_64-unknown-linux-gnu
```

Install a cross-linker. Pick one:

```sh
# Option A: cargo-zigbuild (recommended, easiest)
brew install zig
cargo install cargo-zigbuild

# Option B: cross (uses Docker)
cargo install cross
```

### 3. LLVM + bpf-linker (for eBPF native builds)

```sh
brew install llvm

PATH="/opt/homebrew/opt/llvm/bin:$PATH" \
LLVM_SYS_200_PREFIX=/opt/homebrew/opt/llvm \
cargo +nightly install --no-default-features --features llvm-20 bpf-linker
```

> If Homebrew installs a newer LLVM (e.g. 21), adjust the feature flag
> (`llvm-21`) and env var (`LLVM_SYS_210_PREFIX`) accordingly.

### 4. Deploy targets

Create `.env.targets` in the repo root:

```sh
TARGETS="root@your-server-ip"
SSH_PORT=22
```

## Deploy

```sh
just deploy
```

This cross-compiles with `--features ebpf-native`, uploads binaries, and restarts `ployzd` on the remote server(s).

## Troubleshooting

**`unable to find LLVM shared lib`** — bpf-linker was installed with default features (uses rustc's LLVM proxy). Reinstall with `--no-default-features --features llvm-20` as shown above.

**`package ID specification 'ployz-ebpf' did not match any packages`** — the `ebpf` crate must be in the workspace `members` list in `Cargo.toml`.

**`bpf-linker: SIGABRT`** — LLVM version mismatch. Check `brew info llvm` for your version and match the feature flag.
