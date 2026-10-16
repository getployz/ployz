import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { describe, expect, it } from "vitest";

const SRC = join(process.cwd(), "src");
const WRITER = "modules/web-analytics/clickhouse/attempt.ts";

// Every ClickHouse row reaches a table through `attempt.ts`, which stamps it with an Attempt that Postgres
// publishes only after the write is acknowledged. The schema DDL and the test database setup are the only other
// callers of the unchecked client, and neither writes rows.
const UNCHECKED_WRITERS = [WRITER, "modules/web-analytics/clickhouse/schema.ts", "test/clickhouse.ts"];

const UNCHECKED_CALL = /\.unchecked\.(insert|command)\(/;
const SEQUENCE = /nextval\(\s*'analytics\.clickhouse_attempt'/;
const FORGED_BRAND = /\bas\s+(Attempt|Acknowledged)\b|<(Attempt|Acknowledged)>/;
const IMPORTS_WRITER = /from "(#\/modules\/web-analytics\/clickhouse\/attempt|\.\/attempt)"/;

function walk(dir: string): string[] {
  const files: string[] = [];
  for (const entry of readdirSync(dir)) {
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) files.push(...walk(path));
    else if (path.endsWith(".ts") || path.endsWith(".tsx")) files.push(path);
  }
  return files;
}

const isTest = (rel: string) => /\.test\.tsx?$/.test(rel);

const hits = (pattern: RegExp, keep: (rel: string, source: string) => boolean) =>
  walk(SRC).flatMap((path) => {
    const rel = relative(SRC, path).replaceAll("\\", "/");
    const source = readFileSync(path, "utf8");
    if (!keep(rel, source) || rel.endsWith(".static.test.ts")) return [];
    return source.split("\n").flatMap((line, index) => (pattern.test(line) ? [`${rel}:${index + 1}: ${line.trim()}`] : []));
  });

describe("ClickHouse writes go through the attempt API", () => {
  it("only the writer module, the schema DDL and the test database setup call the unchecked client", () => {
    const callers = [...new Set(hits(UNCHECKED_CALL, (rel) => !isTest(rel)).map((hit) => hit.slice(0, hit.indexOf(":"))))].sort();
    expect(callers).toEqual([...UNCHECKED_WRITERS].sort());
  });

  it("only the writer module draws from the attempt sequence", () => {
    expect(hits(SEQUENCE, (rel) => rel !== WRITER)).toEqual([]);
  });

  it("nothing that uses the writer module forges an Attempt or an Acknowledged proof", () => {
    expect(hits(FORGED_BRAND, (rel, source) => rel !== WRITER && IMPORTS_WRITER.test(source))).toEqual([]);
  });
});
