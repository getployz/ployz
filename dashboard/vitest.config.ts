import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";

const sharedTestConfig = {
  setupFiles: ["./src/test/setup-env.ts", "./vitest.setup.ts"],
  server: {
    deps: {
      inline: ["react", "react-dom"],
    },
  },
};

export default defineConfig({
  resolve: {
    alias: {
      "#": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  test: {
    projects: [
      {
        test: {
          ...sharedTestConfig,
          name: "unit",
          include: ["src/**/*.test.{ts,tsx}"],
          exclude: ["**/*.postgres.test.ts", "**/*.clickhouse.test.ts", "**/*.static.test.ts"],
          maxWorkers: 4,
          isolate: false,
        },
      },
      {
        resolve: {
          alias: {
            // The build-time entries TanStack Start's Vite plugin provides, so a test can dispatch a request the way the
            // app's server does.
            "#tanstack-router-entry": fileURLToPath(new URL("./src/router.tsx", import.meta.url)),
            "#tanstack-start-entry": fileURLToPath(new URL("./src/start.ts", import.meta.url)),
          },
        },
        test: {
          ...sharedTestConfig,
          // Inlined so start-server-core's imports of those entries resolve through the aliases above.
          server: { deps: { inline: [...sharedTestConfig.server.deps.inline, /@tanstack\/(react-start|react-start-server|start-server-core)\//] } },
          name: "postgres",
          include: ["src/**/*.postgres.test.ts"],
          // One server for the run; each file gets its own copy of the migrated database.
          globalSetup: ["./src/test/postgres.global-setup.ts"],
          maxWorkers: 4,
        },
      },
      {
        test: {
          ...sharedTestConfig,
          name: "clickhouse",
          include: ["src/**/*.clickhouse.test.ts"],
          // One server of each for the run; each file gets its own database on both.
          globalSetup: ["./src/test/postgres.global-setup.ts", "./src/test/clickhouse.global-setup.ts"],
          maxWorkers: 4,
        },
      },
      {
        test: {
          ...sharedTestConfig,
          name: "eval",
          // The agent eval, run only by hand (`pnpm eval`): never in `pnpm test` or CI. Live trials need `pnpm eval:live`.
          include: ["evals/**/*.eval.ts", "evals/**/*.test.ts"],
          globalSetup: ["./src/test/postgres.global-setup.ts"],
          maxWorkers: 4,
          testTimeout: 60_000,
        },
      },
      {
        test: {
          ...sharedTestConfig,
          name: "static-analysis",
          include: ["src/**/*.static.test.ts"],
          maxWorkers: 1,
          testTimeout: 120_000,
        },
      },
    ],
  },
});
