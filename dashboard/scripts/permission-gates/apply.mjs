// Rewrites each gate in sites.tsv from its old need to its permission. Lines are as of a85846a83, where Need still existed.
// Run from dashboard/: node scripts/permission-gates/apply.mjs
import { readFileSync, writeFileSync } from "node:fs";

const [, ...rows] = readFileSync(new URL("./sites.tsv", import.meta.url), "utf8").trimEnd().split("\n").map((line) => line.split("\t"));
const files = new Map();
for (const [file, line, site, need, permission] of rows) {
  if (!permission.includes(":")) continue;
  const lines = files.get(file) ?? readFileSync(file, "utf8").split("\n");
  files.set(file, lines);
  const index = Number(line) - 1;
  if (!lines[index].includes(`"${need}"`)) throw new Error(`${file}:${line} (${site}) does not ask for "${need}"`);
  lines[index] = lines[index].replace(`"${need}"`, `"${permission}"`);
}
for (const [file, lines] of files) writeFileSync(file, lines.join("\n"));
