# ployz2

Ployz runs containerized Services across a Cluster of your own Docker Machines.

## Install

```sh
curl -fsSL https://ployz.sh | sh
brew install getployz/ployz/ployz
```

Release process: [docs/RELEASE.md](docs/RELEASE.md).

## Workspace

- `ployz-core`: domain and wire contracts shared by both binaries
- `ployz`: CLI for Linux, macOS, and Windows through WSL
- `ployzd`: Linux-only daemon
- `ployz-testkit`: unpublished support crate used only by tests

Run the fast local gate with `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and `cargo test --workspace --all-features`.
