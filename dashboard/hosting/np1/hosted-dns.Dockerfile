# Builds getployz/hosted-dns (Railway used Railpack). Context: the hosted-dns checkout.
FROM rust:1.97.1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked && cp target/release/hosted-dns /hosted-dns

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/*
COPY --from=build /hosted-dns /usr/local/bin/hosted-dns
USER nobody
EXPOSE 8080
CMD ["/usr/local/bin/hosted-dns"]
