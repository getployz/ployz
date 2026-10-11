# syntax=docker/dockerfile:1
# Build from the repo root: docker build -f dashboard/uptime/Dockerfile .
# The crate path-depends on ployz-core, which inherits from core/Cargo.toml.
FROM rust:1.97.1-bookworm AS chef
RUN cargo install cargo-chef --version 0.1.78 --locked \
    && rm -rf /usr/local/cargo/registry /usr/local/cargo/git
WORKDIR /app/dashboard/uptime

FROM chef AS planner
COPY core/Cargo.toml /app/core/Cargo.toml
COPY core/crates/ployz-core/ /app/core/crates/ployz-core/
COPY dashboard/uptime/ ./
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS build
# No cache mounts, as in the root Dockerfile: compiled dependencies live in a layer.
# cargo-chef skips path dependencies outside the crate's workspace, so ployz-core goes in
# whole and cooks with the registry crates.
COPY core/Cargo.toml /app/core/Cargo.toml
COPY core/crates/ployz-core/ /app/core/crates/ployz-core/
COPY --from=planner /app/dashboard/uptime/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json
COPY dashboard/uptime/ ./
RUN cargo build --release --locked --bin ployz-uptime

# Probe and Postgres TLS trust webpki-roots, so the runtime needs no CA bundle.
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f
COPY --from=build /app/dashboard/uptime/target/release/ployz-uptime /usr/local/bin/ployz-uptime
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/ployz-uptime"]
CMD ["all"]
