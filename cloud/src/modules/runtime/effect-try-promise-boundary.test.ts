import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { describe, expect, it } from "vitest";

const SRC = join(process.cwd(), "src");

const TRY_PROMISE_ALLOWLIST = new Set([
  "modules/runtime/ployz.server.ts",
  "modules/billing/polar-provider.server.ts",
  "modules/github/github-observation.api.ts",
  "server/auth.server.ts",
  "server/database.server.ts",
  "modules/inngest/client.ts",
  "modules/inngest/http.ts",
  "modules/environment-design/workspace-bootstrap.server.ts",
  "routes/api/enroll/-handlers.ts",
  "routes/api/auth/github.ts",
  "routes/api/github/-webhook.handler.ts",
]);

function walk(dir: string): string[] {
  const entries = readdirSync(dir);
  const files: string[] = [];
  for (const entry of entries) {
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) {
      files.push(...walk(path));
      continue;
    }
    if (path.endsWith(".ts") || path.endsWith(".tsx")) files.push(path);
  }
  return files;
}

describe("Effect.tryPromise call-site boundary", () => {
  it("keeps tryPromise inside provider and HTTP boundaries", () => {
    const hits = walk(SRC).flatMap((path) => {
      const rel = relative(SRC, path).replaceAll("\\", "/");
      if (
        rel.endsWith(".test.ts") ||
        rel.endsWith(".test.tsx") ||
        rel.endsWith(".postgres.test.ts")
      ) {
        return [];
      }
      if (TRY_PROMISE_ALLOWLIST.has(rel)) return [];
      const source = readFileSync(path, "utf8");
      const lines = source.split("\n");
      return lines.flatMap((line, index) =>
        line.includes("Effect.tryPromise")
          ? [`${rel}:${index + 1}: ${line.trim()}`]
          : [],
      );
    });
    expect(hits).toEqual([]);
  });
});
