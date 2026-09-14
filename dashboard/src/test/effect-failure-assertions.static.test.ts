import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { describe, expect, it } from "vitest";

const SRC = join(process.cwd(), "src");

// Unit tests assert failures with `Effect.flip` and an `instanceof` check on
// the typed error, not by digging into an Exit's cause. Postgres suites are
// excluded until they migrate to `@effect/vitest`.
// `Result.isFailure` is not matched: Result is a value type and is asserted
// directly by design.
const EXIT_DIGGING =
  /Exit\.isFailure\(|\w+\._tag\s*===\s*"Failure"|\w+\._tag,\s*"Failure"\)/;

// Temporary baseline: this file is edited by two open PRs; its `_tag`
// assertions convert once they merge. Remove the entry with that conversion.
const DEFERRED = new Set([
  "modules/runtime/organization-runtime.server.effect.test.ts",
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
    if (path.endsWith(".test.ts") || path.endsWith(".test.tsx")) files.push(path);
  }
  return files;
}

describe("Effect failure assertions in unit tests", () => {
  it("assert typed failures through Effect.flip instead of Exit digging", () => {
    const hits = walk(SRC).flatMap((path) => {
      const rel = relative(SRC, path).replaceAll("\\", "/");
      if (rel.endsWith(".postgres.test.ts") || rel.endsWith(".static.test.ts")) return [];
      if (DEFERRED.has(rel)) return [];
      const lines = readFileSync(path, "utf8").split("\n");
      return lines.flatMap((line, index) =>
        EXIT_DIGGING.test(line) ? [`${rel}:${index + 1}: ${line.trim()}`] : [],
      );
    });
    expect(hits).toEqual([]);
  });
});
