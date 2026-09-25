# syntax=docker/dockerfile:1
FROM node:24.18.1-bookworm-slim AS node

FROM rust:1.97.1-bookworm AS sdk
COPY --from=node /usr/local/bin/node /usr/local/bin/node
WORKDIR /app
# Dashboard edits must not invalidate this layer.
COPY core/ core/
# Railway requires its literal service ID; BuildKit on Ployz accepts the same cache IDs.
RUN --mount=type=cache,id=s/8089d161-49d3-4c77-b683-2bede5a784f5-/usr/local/cargo/registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=s/8089d161-49d3-4c77-b683-2bede5a784f5-/usr/local/cargo/git,target=/usr/local/cargo/git \
    --mount=type=cache,id=s/8089d161-49d3-4c77-b683-2bede5a784f5-/app/core/target,target=/app/core/target \
    bash core/scripts/build-cloud-sdk.sh

FROM node AS dashboard
RUN npm install --global pnpm@11.7.0
WORKDIR /app/dashboard
COPY dashboard/package.json dashboard/pnpm-lock.yaml dashboard/pnpm-workspace.yaml ./
COPY dashboard/patches/ patches/
COPY --from=sdk /app/core/crates/ployz-sdk /app/core/crates/ployz-sdk
RUN --mount=type=cache,id=s/8089d161-49d3-4c77-b683-2bede5a784f5-/root/.local/share/pnpm/store,target=/root/.local/share/pnpm/store pnpm install --frozen-lockfile
COPY dashboard/ ./
# The SDK was built above; do not run package.json's combined Rust + Vite build.
RUN pnpm build:app && pnpm prune --prod

FROM node AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git openssh-client \
    && rm -rf /var/lib/apt/lists/*
ENV NODE_ENV=production
WORKDIR /app
COPY --from=dashboard /app/dashboard/.output .output
COPY --from=dashboard /app/dashboard/node_modules node_modules
# The app's packaged SDK is the runtime copy; replace pnpm's source-tree link.
RUN ln -sfn /app/.output/server/node_modules/@ployz/sdk node_modules/@ployz/sdk
COPY dashboard/package.json dashboard/drizzle.config.ts ./
COPY dashboard/drizzle/ drizzle/
# Load the OpenTelemetry SDK before the app; it reads Railway's OTEL_* variables.
CMD ["node", "--experimental-loader=@opentelemetry/instrumentation/hook.mjs", "--import", "@opentelemetry/auto-instrumentations-node/register", ".output/server/index.mjs"]
