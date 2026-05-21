default:
    @just --list

fmt:
    cargo fmt --all --check

test:
    cargo test --workspace

boundary:
    ./scripts/check-boundary.sh

check:
    just fmt
    just test
    just boundary
