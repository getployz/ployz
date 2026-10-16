import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { describe, expect, it } from "vitest";

const SRC = join(process.cwd(), "src");
const ATTEMPT_MODULE = "modules/web-analytics/clickhouse/attempt.ts";
const SITE_TABLE_MODULE = "modules/web-analytics/tables.ts";

const UNCHECKED_ATTEMPT_MODULES = [ATTEMPT_MODULE, "modules/web-analytics/clickhouse/schema.ts", "test/clickhouse.ts"];

const UNCHECKED_CALL = /\.unchecked\.(insert|command|select)\(/;
const SEQUENCE = /nextval\(\s*'analytics\.clickhouse_attempt'/;
const FORGED_BRAND = /\bas\s+Attempt\b|<Attempt>/;
const SITE_TABLE = /clickhouse_site|analyticsClickhouseSite/;
const VISIBILITY_COLUMN = /\b(floor|below|pending|generation|days_dates|days_attempts)\b|analyticsClickhouseSite\.(floor|below|pending|days|generation)\b/;
const IMPORTS_ATTEMPT_MODULE = /from "(#\/modules\/web-analytics\/clickhouse\/attempt|\.\/attempt)"/;

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

const statements = (keep: (rel: string) => boolean) =>
  walk(SRC).flatMap((path) => {
    const rel = relative(SRC, path).replaceAll("\\", "/");
    if (!keep(rel) || isTest(rel)) return [];
    return [...readFileSync(path, "utf8").matchAll(/sql`([^`]*)`/g)].map((match) => ({ rel, text: match[1] ?? "" }));
  });

describe("ClickHouse writes go through the attempt API", () => {
  it("only the attempt module, the schema DDL and the test database setup call the unchecked client", () => {
    const callers = [...new Set(hits(UNCHECKED_CALL, (rel) => !isTest(rel)).map((hit) => hit.slice(0, hit.indexOf(":"))))].sort();
    expect(callers).toEqual([...UNCHECKED_ATTEMPT_MODULES].sort());
  });

  it("only the attempt module draws from the attempt sequence", () => {
    expect(hits(SEQUENCE, (rel) => rel !== ATTEMPT_MODULE)).toEqual([]);
  });

  it("nothing that uses the attempt module forges an Attempt", () => {
    expect(hits(FORGED_BRAND, (rel, source) => rel !== ATTEMPT_MODULE && IMPORTS_ATTEMPT_MODULE.test(source))).toEqual([]);
  });

  it("only the attempt module reads or writes the site's visibility columns, so a snapshot exists only inside read or locked", () => {
    const touchesSiteTable = (rel: string, source: string) =>
      !isTest(rel) && rel.startsWith("modules/web-analytics/") && rel !== ATTEMPT_MODULE && rel !== SITE_TABLE_MODULE && SITE_TABLE.test(source);
    expect(hits(VISIBILITY_COLUMN, touchesSiteTable)).toEqual([]);
  });

  it("no statement sorts by a column it also aliases a cast onto, so order by never resolves to the text alias", () => {
    expect(statements((rel) => rel.startsWith("modules/web-analytics/")).flatMap(({ rel, text }) => {
      const sorts = [...text.matchAll(/\border by\s+(?:[\w.]+\.)?(\w+)/g)].map((match) => match[1] ?? "");
      return sorts.some((column) => new RegExp(`\\b\\w+::\\w+\\s+as\\s+${column}\\b`).test(text)) ? [`${rel}: ${text.replaceAll(/\s+/g, " ").trim()}`] : [];
    })).toEqual([]);
  });
});
