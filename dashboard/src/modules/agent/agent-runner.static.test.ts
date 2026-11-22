import { readFileSync } from "node:fs";
import { parseSync, Visitor } from "oxc-parser";
import { expect, it } from "vitest";

const file = new URL("./agent-runner.server.ts", import.meta.url);
const parsed = parseSync(file.pathname, readFileSync(file, "utf8"));

const named = new Set<string>();
new Visitor({ Identifier: (node) => { named.add(node.name); } }).visit(parsed.program);

const runtimeImportsFrom = (pattern: RegExp) => parsed.module.staticImports
  .filter((entry) => pattern.test(entry.moduleRequest.value))
  .flatMap((entry) => entry.entries.filter((name) => !name.isType).map((name) => name.importName.name ?? ""));

// An approved proposal's bundle is the approver's browser signature. The runner that answers an agent holds none of it:
// it creates, stages and presents a proposal and reads its status, so no call of an agent's can carry the bundle out.
it("the agent runner takes nothing from a proposal but its status, never the bundle the approver signed", () => {
  expect(parsed.errors).toEqual([]);
  expect(runtimeImportsFrom(/(^|\/)proposals\/proposals\.server$/u).sort()).toEqual(["createProposal", "presentProposal", "proposalStatus", "stageStep"]);
  expect(runtimeImportsFrom(/(^|\/)proposals\/tables$/u)).toEqual([]);
  expect([...named].filter((name) => /bundle|operationProposals/iu.test(name))).toEqual([]);
});
