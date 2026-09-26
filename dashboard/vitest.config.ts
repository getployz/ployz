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
          exclude: ["**/*.postgres.test.ts", "**/*.static.test.ts"],
          maxWorkers: 4,
          isolate: false,
        },
      },
      {
        test: {
          ...sharedTestConfig,
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
          name: "static-analysis",
          include: ["src/**/*.static.test.ts"],
          maxWorkers: 1,
          testTimeout: 120_000,
        },
      },
    ],
  },
});
