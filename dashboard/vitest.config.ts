import { statSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";
import { BaseSequencer, type TestSpecification } from "vitest/node";

// Vitest's shards hold equal file counts, so CI's slowest shard ran three times as long as its fastest.
// Source size tracks a file's run time, so each shard here takes the largest remaining file while it holds the fewest bytes.
class SizeBalancedSequencer extends BaseSequencer {
  override async shard(files: TestSpecification[]): Promise<TestSpecification[]> {
    const { shard } = this.ctx.config;
    if (!shard) return files;
    const key = (spec: TestSpecification) => `${spec.project.name}:${spec.moduleId}`;
    const sized = files
      .map((spec) => ({ spec, bytes: statSync(spec.moduleId).size }))
      .sort((a, b) => b.bytes - a.bytes || (key(a.spec) < key(b.spec) ? -1 : 1));
    const shards: { bytes: number; files: TestSpecification[] }[] = Array.from({ length: shard.count }, () => ({
      bytes: 0,
      files: [],
    }));
    for (const { spec, bytes } of sized) {
      const lightest = shards.reduce((min, next) => (next.bytes < min.bytes ? next : min));
      lightest.bytes += bytes;
      lightest.files.push(spec);
    }
    return shards[shard.index - 1]?.files ?? [];
  }
}

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
    sequence: { sequencer: SizeBalancedSequencer },
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
