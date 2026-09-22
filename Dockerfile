# syntax=docker/dockerfile:1
FROM node:24.18.1-bookworm-slim AS node

FROM golang:1.26.1-bookworm AS go

FROM rust:1.97.1-bookworm AS sdk
COPY --from=go /usr/local/go /usr/local/go
ENV PATH="/usr/local/go/bin:${PATH}"
COPY --from=node /usr/local/bin/node /usr/local/bin/node
WORKDIR /app
# Dashboard edits must not invalidate this layer.
COPY core/ core/
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/core/target \
    bash core/scripts/build-cloud-sdk.sh

FROM node AS dashboard
RUN npm install --global pnpm@11.7.0
WORKDIR /app/dashboard
COPY dashboard/package.json dashboard/pnpm-lock.yaml dashboard/pnpm-workspace.yaml ./
COPY dashboard/patches/ patches/
COPY --from=sdk /app/core/crates/ployz-sdk /app/core/crates/ployz-sdk
RUN --mount=type=cache,target=/root/.local/share/pnpm/store pnpm install --frozen-lockfile
COPY dashboard/ ./
# The SDK was built above; do not run package.json's combined Rust + Vite build.
RUN pnpm exec vite build && node scripts/package-sdk.mjs && pnpm prune --prod

FROM node AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git openssh-client \
    && rm -rf /var/lib/apt/lists/*
ENV NODE_ENV=production
WORKDIR /app
COPY --from=dashboard /app/dashboard/.output dashboard/.output
COPY --from=dashboard /app/dashboard/node_modules dashboard/node_modules
COPY --from=dashboard /app/core/crates/ployz-sdk core/crates/ployz-sdk
COPY dashboard/package.json dashboard/drizzle.config.ts dashboard/
COPY dashboard/drizzle/ dashboard/drizzle/
COPY dashboard/scripts/start-with-inngest-sync.mjs dashboard/scripts/start-with-inngest-sync.mjs
# Keep /app as the working directory for Railway's existing cd dashboard commands.
CMD ["sh", "-c", "cd dashboard && exec node scripts/start-with-inngest-sync.mjs"]
