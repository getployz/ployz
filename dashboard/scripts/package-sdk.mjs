import { cpSync, mkdirSync, readFileSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const source = new URL("../../core/crates/ployz-sdk/", import.meta.url);
const destination = new URL("../.output/server/node_modules/@ployz/sdk/", import.meta.url);
mkdirSync(destination, { recursive: true });
const manifest = JSON.parse(readFileSync(new URL("package.json", source), "utf8"));
for (const file of ["package.json", ...manifest.files, "ployz-sdk.node", "ployz-tailcat"]) {
  cpSync(new URL(file, source), new URL(file, destination), { recursive: true });
}
// Rung 1: exercise the shipped native binding, outside the source package.
const require = createRequire(new URL("../.output/server/index.mjs", import.meta.url));
const { parseServiceSetting } = require("@ployz/sdk/config");
assert.deepEqual(parseServiceSetting("source", { version: 1, type: "empty", rootDir: "/" }),
  { version: 1, type: "empty", rootDir: "/" });

const helper = new URL("ployz-tailcat", destination);
assert.ok(statSync(helper).mode & 0o111, "packaged helper must be executable");
assert.equal(execFileSync(fileURLToPath(helper), ["--version"], { encoding: "utf8", timeout: 5000 }).trim(), manifest.version);
