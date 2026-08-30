# ployz2

CLI and daemon for a cluster of Docker machines

## Install

```sh
curl -fsSL https://ployz.sh | sh
brew install getployz/ployz/ployz
```

Release process: [docs/RELEASE.md](docs/RELEASE.md).

## Workspace

- `ployz-core`: domain and wire contracts shared by both binaries
- `ployz`: CLI for Linux, macOS, and Windows through WSL
- `ployz-relay`: Cloud Relay HTTP/1.1 WebSocket splice (Linux binary + `ghcr.io/getployz/ployz-relay`)
- `ployz-sdk`: napi package `@ployz/sdk` (linux and macOS x64/arm64 gnu bindings; published on GitHub Release Publish)
- `ployz-sdk-payloads`: Rust-sourced TypeScript declarations for `@ployz/sdk`
- `ployzd`: Linux-only daemon
- `ployz-testkit`: unpublished support crate used only by tests

Run the fast local gate with `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and `cargo test --workspace --all-features`.
