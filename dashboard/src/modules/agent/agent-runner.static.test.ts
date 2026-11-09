import { readFileSync } from "node:fs";
import { expect, it } from "vitest";

const source = readFileSync(new URL("./agent-runner.server.ts", import.meta.url), "utf8");

// An approved proposal's bundle is the approver's browser signature. The runner that answers an agent holds none of it:
// it creates, stages and presents a proposal and reads its status, so no call of an agent's can carry the bundle out.
it("the agent runner takes nothing from a proposal but its status, never the bundle the approver signed", () => {
  expect(source).not.toMatch(/\bbundle\b/i);
  expect(source).not.toContain("operationProposals");
  const imported = /import \{([^}]*)\} from "#\/modules\/proposals\/proposals\.server";/.exec(source)?.[1] ?? "";
  expect(imported.split(",").map((name) => name.trim()).sort()).toEqual(["createProposal", "presentProposal", "proposalStatus", "stageStep"]);
});
