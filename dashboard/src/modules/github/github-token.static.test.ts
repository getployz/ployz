import { readdirSync, readFileSync } from "node:fs";
import { join, relative } from "node:path";
import { expect, it } from "vitest";

const SRC = join(process.cwd(), "src");

it("mints installation tokens in github-token.server.ts only", () => {
  const minting = readdirSync(SRC, { recursive: true, withFileTypes: true })
    .filter((entry) => entry.isFile() && /\.tsx?$/.test(entry.name) && !/\.test\.tsx?$/.test(entry.name))
    .map((entry) => relative(SRC, join(entry.parentPath, entry.name)))
    .filter((file) => readFileSync(join(SRC, file), "utf8").includes("/access_tokens"));
  expect(minting).toEqual(["modules/github/github-token.server.ts"]);
});
