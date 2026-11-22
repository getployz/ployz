import { readFileSync } from "node:fs";
import { parseSync } from "oxc-parser";
import { expect, it } from "vitest";

const file = new URL("./agent-runner.server.ts", import.meta.url);
const parsed = parseSync(file.pathname, readFileSync(file, "utf8"));

/** Every name the runner spells: identifiers, property keys and member accesses. */
function names(node: unknown, found = new Set<string>()): Set<string> {
  if (Array.isArray(node)) for (const child of node) names(child, found);
  else if (node !== null && typeof node === "object") {
    const record = node as Record<string, unknown>;
    if (typeof record["name"] === "string" && typeof record["type"] === "string") found.add(record["name"]);
    for (const value of Object.values(record)) names(value, found);
  }
  return found;
}

const runtimeImportsFrom = (pattern: RegExp) => parsed.module.staticImports
  .filter((entry) => pattern.test(entry.moduleRequest.value))
  .flatMap((entry) => entry.entries.filter((name) => !name.isType).map((name) => name.importName.name ?? ""));

// An approved proposal's bundle is the approver's browser signature. The runner that answers an agent holds none of it:
// it creates, stages and presents a proposal and reads its status, so no call of an agent's can carry the bundle out.
it("the agent runner takes nothing from a proposal but its status, never the bundle the approver signed", () => {
  expect(parsed.errors).toEqual([]);
  expect(runtimeImportsFrom(/(^|\/)proposals\/proposals\.server$/u).sort()).toEqual(["createProposal", "presentProposal", "proposalStatus", "stageStep"]);
  expect(runtimeImportsFrom(/(^|\/)proposals\/tables$/u)).toEqual([]);
  expect([...names(parsed.program)].filter((name) => /bundle|operationProposals/iu.test(name))).toEqual([]);
});
