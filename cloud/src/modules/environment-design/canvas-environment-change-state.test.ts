import { expect, it } from "vitest";
import { parseServiceConfig } from "@ployz/sdk/config";
import { buildCanvasEnvironmentChangeState } from "./canvas-environment-change-state";
import type { EnvironmentDeploymentStatus } from "#/modules/deployments/tables";
import type { EnvironmentStateProjection } from "./environment-change-set";

const node = { type: "service" as const, id: "api" };
const config = (replicas: number) => parseServiceConfig({
  version: 2, name: "API", privateDns: "api", replicas,
  source: { version: 1, type: "empty", rootDir: "/" },
  healthcheck: { type: "none" }, restartPolicy: "unless-stopped",
});
const state = (replicas: number): EnvironmentStateProjection => ({
  token: String(replicas), nodes: [{ node, config: config(replicas) }],
});
const empty = { token: "none", nodes: [] };

it.each([
  ["queued", 5, 1, []], ["planning", 5, 1, []], ["deploying", 5, 1, []],
  ["queued", 7, 1, [["5", "7"]]], ["deploying", 7, 1, [["5", "7"]]],
  ["failed", 5, 1, [["1", "5"]]], ["failed", 7, 1, [["1", "7"]]],
  ["cancelled", 7, 1, [["1", "7"]]], ["applied", 5, 5, []], ["applied", 7, 5, [["5", "7"]]],
] as Array<[EnvironmentDeploymentStatus, number, number, string[][]]>)(
  "shows one diff for %s with Working %s and Applied %s", (status, working, applied, expected) => {
    const result = buildCanvasEnvironmentChangeState({
      working: state(working), saved: state(5), applied: state(applied),
      nodeIntroductions: empty,
      deploymentEvidence: { id: "attempt", status, ...state(5) },
      nodes: [{ node, name: "API", summaryLabel: "API" }],
    });
    expect(result.groups.flatMap(group => group.rows.map(row => [row.currentValue, row.newValue]))).toEqual(expected);
    expect(result.totalCount).toBe(expected.length);
    expect(result.canSave).toBe(working !== 5);
  },
);

it("keeps the submitted revision as baseline after a later Save", () => {
  const result = buildCanvasEnvironmentChangeState({
    working: state(7), saved: state(9), applied: state(1), nodeIntroductions: empty,
    deploymentEvidence: { id: "attempt", status: "queued", ...state(5) },
    nodes: [],
  });
  expect(result.groups[0]?.rows).toMatchObject([{ currentValue: "5", newValue: "7", canDiscard: true }]);
});

it("advances successful nodes independently after a partial failure", () => {
  const worker = { type: "service" as const, id: "worker" };
  const target = { token: "target", nodes: [...state(5).nodes, { node: worker, config: config(5) }] };
  const result = buildCanvasEnvironmentChangeState({
    working: target, saved: target,
    applied: { token: "partial", nodes: [...state(5).nodes, { node: worker, config: config(1) }] },
    nodeIntroductions: empty, deploymentEvidence: null, nodes: [],
  });
  expect(result.groups).toMatchObject([{ nodeId: "worker", rows: [{ currentValue: "1", newValue: "5" }] }]);
  expect(result.totalCount).toBe(1);
});
