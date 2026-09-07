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
          sequence: { groupOrder: 0 },
        },
      },
      {
        test: {
          ...sharedTestConfig,
          name: "postgres",
          include: ["src/**/*.postgres.test.ts"],
          maxWorkers: 1,
          sequence: { groupOrder: 1 },
        },
      },
      {
        test: {
          ...sharedTestConfig,
          name: "static-analysis",
          include: ["src/**/*.static.test.ts"],
          maxWorkers: 1,
          testTimeout: 120_000,
          sequence: { groupOrder: 2 },
        },
      },
    ],
  },
});
