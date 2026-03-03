build:
    cargo build

test:
    cargo test

ployz *args:
    cargo run --bin ployz -- {{args}}

ployzd *args:
    cargo run --bin ployzd -- {{args}}