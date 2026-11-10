import { readdirSync, readFileSync } from "node:fs";
import { join, relative } from "node:path";
import { expect, it } from "vitest";

const SRC = join(process.cwd(), "src");
const AUTHORITY = "modules/github/github-authority.server.ts";

const sources = readdirSync(SRC, { recursive: true, withFileTypes: true })
  .filter((entry) => entry.isFile() && /\.tsx?$/.test(entry.name) && !/\.test\.tsx?$/.test(entry.name))
  .map((entry) => relative(SRC, join(entry.parentPath, entry.name)))
  .map((file) => ({ file, text: readFileSync(join(SRC, file), "utf8") }));

const matching = (pattern: RegExp) => sources.filter(({ text }) => pattern.test(text)).map(({ file }) => file).sort();

it("reads the App's key and client secret only where the App's authority is held", () => {
  expect(matching(/\bapp(PrivateKey|ClientSecret)\b/)).toEqual([AUTHORITY, "server/config.server.ts"]);
});

it("mints tokens and exchanges sign-in codes only through the App's authority", () => {
  expect(matching(/\/access_tokens\b|\/login\/oauth\/access_token\b/)).toEqual([AUTHORITY]);
});

it("names GitHub Actions only in the App's authority", () => {
  expect(matching(/\/actions\//)).toEqual([AUTHORITY]);
});

it("writes to GitHub only through the App's authority", () => {
  const writing = sources
    .filter(({ text }) => /origins\.api\b|api\.github\.com/.test(text) && /method\s*:\s*["'`](POST|PUT|PATCH|DELETE)\b/i.test(text))
    .map(({ file }) => file);
  expect(writing).toEqual([AUTHORITY]);
});
