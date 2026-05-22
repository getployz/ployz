default:
    @just --list

fmt:
    cargo fmt --all --check

test:
    cargo test --workspace

boundary:
    ./scripts/check-boundary.sh
    ./scripts/check-boundary.sh --self-test

check:
    just fmt
    just test
    just boundary
