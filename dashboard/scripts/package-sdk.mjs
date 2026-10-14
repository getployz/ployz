import { cpSync, mkdirSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";
import assert from "node:assert/strict";

const source = new URL("../../core/crates/ployz-sdk/", import.meta.url);
const destination = new URL("../.output/server/node_modules/@ployz/sdk/", import.meta.url);
mkdirSync(destination, { recursive: true });
const manifest = JSON.parse(readFileSync(new URL("package.json", source), "utf8"));
for (const file of ["package.json", ...manifest.files, "ployz-sdk.node"]) {
  cpSync(new URL(file, source), new URL(file, destination), { recursive: true });
}
// Rung 1: load the shipped WASM config and native RPC binding, outside the source package.
const require = createRequire(new URL("../.output/server/index.mjs", import.meta.url));
assert.equal(typeof require("@ployz/sdk").connect, "function");
assert.equal(typeof require("@ployz/sdk").openConfigStore, "function");
// Node finds ESM named imports by scanning index.js, and the scan stops at the first export whose value is not a bare name.
const named = Object.keys(await import(pathToFileURL(require.resolve("@ployz/sdk")).href)).filter((key) => key !== "default" && key !== "module.exports");
assert.deepEqual(named.sort(), Object.keys(require("@ployz/sdk")).sort());
const { parseServiceSetting } = require("@ployz/sdk/config");
assert.deepEqual(parseServiceSetting("source", { version: 1, type: "empty", rootDir: "/" }),
  { version: 1, type: "empty", rootDir: "/" });
