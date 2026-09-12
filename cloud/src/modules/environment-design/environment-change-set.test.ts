import { expect, it } from "vitest";
import { parseServiceConfig } from "@ployz/sdk/config";
import { buildEnvironmentChangeSet, type EnvironmentNodeProjection } from "./environment-change-set";

const service = parseServiceConfig({
  version: 2, name: "API", privateDns: "api",
  source: { version: 1, type: "empty", rootDir: "/" },
  healthcheck: { type: "none" }, restartPolicy: "unless-stopped",
});
const empty = { token: "none", nodes: [] };

it.each([
  { type: "service", config: service },
  { type: "variable_group", config: { version: 1, name: "Variables", variables: [] } },
  { type: "volume", config: { version: 2, name: "Data" } },
] as const)("counts $type lifecycle once and suppresses submitted creations/deletions", ({ type, config }) => {
  const entry = { node: { type, id: "node" }, config } as EnvironmentNodeProjection;
  const present = { token: "present", nodes: [entry] };
  for (const [working, applied, lifecycle] of [[present, empty, "create"], [empty, present, "delete"]] as const) {
    const pending = buildEnvironmentChangeSet({ working, saved: working, applied, submitted: null, nodeIntroductions: empty });
    expect(pending.groups).toMatchObject([{ node: entry.node, lifecycle }]);
    expect(pending.totalCount).toBe(1);
    const submitted = buildEnvironmentChangeSet({ working, saved: working, applied, submitted: working, nodeIntroductions: empty });
    expect(submitted.groups).toEqual([]);
  }
});

it("uses Introduction for new-node field resets without hiding the creation", () => {
  const node = { type: "service" as const, id: "api" };
  const result = buildEnvironmentChangeSet({
    working: { token: "working", nodes: [{ node, config: { ...service, replicas: 7 } }] },
    saved: empty, applied: empty, submitted: null,
    nodeIntroductions: { token: "introduced", nodes: [{ node, config: { ...service, replicas: 1 } }] },
  });
  expect(result.groups).toMatchObject([{ lifecycle: "create", settings: [{ path: "replicas", before: 1, after: 7, canRestore: true }] }]);
  expect(result.totalCount).toBe(2);
});

it("retains secret differences without exposing values or double-counting derived variables", () => {
  const node = { type: "service" as const, id: "api" };
  const before = { ...service, env: { TOKEN: { kind: "secret" as const, fingerprint: "private-before" } } };
  const after = { ...service, env: { TOKEN: { kind: "secret" as const, fingerprint: "private-after" } } };
  const baseline = { token: "applied", nodes: [{ node, config: before }] };
  const input = {
    working: { token: "working", nodes: [{ node, config: after }] },
    saved: baseline, applied: baseline, submitted: null, nodeIntroductions: empty,
  };
  const snapshot = structuredClone(input);
  const result = buildEnvironmentChangeSet(input);
  expect(result.groups[0]?.settings.map(row => row.path)).toEqual(["env.TOKEN"]);
  expect(JSON.stringify(result)).not.toContain("private-");
  expect(input).toEqual(snapshot);
  expect(buildEnvironmentChangeSet(input)).toEqual(result);
});
