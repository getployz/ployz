#!/usr/bin/env node
// Port of the parts of @napi-rs/cli's processIntermediateTypeFile this addon
// uses: napi-derive writes one JSON def per #[napi] item to TYPE_DEF_TMP_PATH.
import { readFileSync, writeFileSync } from "node:fs";

const [input, output] = process.argv.slice(2);
if (!input || !output) throw new Error("usage: native-dts.mjs <type-def-tmp> <out.d.ts>");

const lines = [
  ...new Set(
    readFileSync(input, "utf8")
      .split("\n")
      .map((line) => line.trim())
      .filter(Boolean)
      .map((line) => (line.startsWith("{") ? line : line.split(":").slice(1).join(":"))),
  ),
].map((line) => JSON.parse(line));
if (lines.length === 0) {
  throw new Error(`${input} has no napi type defs: was the addon built with TYPE_DEF_TMP_PATH set?`);
}

const indent = (text) =>
  text
    .split("\n")
    .filter((line) => line.trim())
    .map((line) => (line.trimStart().startsWith("*") ? `   ${line.trim()}` : `  ${line.trim()}`))
    .join("\n");
const byName = (a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0);

const classes = new Map();
const impls = new Map();
const declarations = [];
for (const def of lines.sort(byName)) {
  switch (def.kind) {
    case "struct":
      classes.set(def.name, def);
      break;
    case "impl":
      impls.set(def.name, [...(impls.get(def.name) ?? []), `${def.js_doc}${def.def}`]);
      break;
    case "fn":
      declarations.push(`${def.js_doc}${def.def}`);
      break;
    case "interface":
    case "enum":
      declarations.push(`${def.js_doc}export ${def.kind} ${def.name} {\n${indent(def.def)}\n}`);
      break;
    default:
      throw new Error(`unsupported napi def kind ${def.kind} for ${def.name}`);
  }
}
for (const [name, def] of classes) {
  const body = [def.def, ...(impls.get(name) ?? [])].filter(Boolean).join("\n");
  if (def.original_name && def.original_name !== name) {
    declarations.push(`export type ${def.original_name} = ${name}`);
  }
  declarations.push(`${def.js_doc}export declare class ${name} {\n${indent(body)}\n}`);
}

const globals = new Set([
  "Array",
  "Error",
  "Exclude",
  "Extract",
  "Omit",
  "Partial",
  "Pick",
  "Promise",
  "Readonly",
  "ReadonlyArray",
  "Record",
]);
const code = lines
  .map((def) => def.def)
  .join("\n")
  .replace(/\/\*[\s\S]*?\*\//g, "")
  .replace(/'[^']*'|"[^"]*"/g, "");
const referenced = [...code.matchAll(/\b([A-Z]\w*)\b(?!\s*\??:)/g)]
  .map(([, name]) => name)
  .filter((name) => !globals.has(name) && !classes.has(name));
const imports = [...new Set(referenced)].sort();

writeFileSync(
  output,
  [
    "/* Generated from the addon's #[napi] items by scripts/native-dts.mjs. Do not edit. */",
    imports.length ? `import type {\n${imports.map((name) => `  ${name},`).join("\n")}\n} from "../index";` : "",
    ...declarations,
    "",
  ].join("\n\n"),
);
