import type { DeploymentView, DiffView, JsonValue, RowId, ServiceListing } from "@ployz/sdk";
import { expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import {
  canFixOnBranch, changeGroups, deploymentActions, deploymentByline, deploymentStatusLabel, deploysLabel, focusedService, missingDeployLogs, outcomeReason,
  nodeLight, nodeOutcomeLabel, shownValue, uploadLabel,
} from "./store-deployments";

// The grouping reads only the changes and each Service's id and source.
const diff = asTestDouble<DiffView>()({
  total_count: 3,
  changes: [
    {
      type: "service", id: "s1", name: "web", lifecycle: "update", comparison: "head", data: null,
      settings: [
        { path: "web.replicas", kind: "update", before: 1, after: 2, canRestore: true, row: null },
        { path: "web.env.TOKEN", kind: "add", before: null, after: "abc", canRestore: true, row: null },
        { path: "web.mounts.pg-data", kind: "remove", before: "/data", after: null, canRestore: true, row: null },
      ],
    },
    { type: "volume", id: "v1", row: "v1:node" as RowId, name: "pg-data", lifecycle: "create", comparison: null, data: null, settings: [] },
  ],
});
const services = [asTestDouble<ServiceListing>()({ id: "s1", source: "image" })];

it("groups the Store's review by node, labelling rows from the catalog; a whole node, a Setting, a variable or a mount discards", () => {
  const [web, volume] = changeGroups(diff, services);
  expect(web).toMatchObject({ nodeType: "service", nodeName: "web", lifecycle: "update", canDiscard: true, serviceSourceType: "image", changeCount: 3 });
  expect(web?.rows.map((row) => [row.path, row.label, row.currentValue, row.newValue, row.canDiscard])).toEqual([
    ["web.replicas", "Replicas", "1", "2", true],
    ["web.env.TOKEN", "Environment variable TOKEN", "", "abc", true],
    ["web.mounts.pg-data", "Volume mount pg-data", "/data", "", true],
  ]);
  // A Volume discards by `volumes.NAME`.
  expect(volume).toMatchObject({ nodeType: "volume", lifecycle: "create", canDiscard: true, discardPath: "volumes.pg-data", changeCount: 1, rows: [] });
});

it("lets a deployed Volume's row discard alone when the Store can restore it", () => {
  const [volume] = changeGroups({ ...diff, changes: [{ type: "volume", id: "v1", row: "v1:node" as RowId, name: "store", lifecycle: "update", comparison: null, data: null, settings: [
    { path: "volumes.store.name", kind: "update", before: "pg-data", after: "store", canRestore: true, row: null },
    { path: "volumes.store.storage", kind: "update", before: null, after: { kind: "docker" }, canRestore: false, row: null },
  ] }] }, services);
  expect(volume?.rows.map((row) => [row.path, row.canDiscard])).toEqual([["volumes.store.name", true], ["volumes.store.storage", false]]);
});

it("words a source change that moves only its root directory or its credentials", () => {
  const source = (before: JsonValue, after: JsonValue) => ({ path: "web.source", kind: "update" as const, before, after, canRestore: true, row: null });
  const [web] = changeGroups({ ...diff, changes: [{ type: "service", id: "s1", row: "s1:node" as RowId, name: "web", lifecycle: "update", comparison: "head", data: null, settings: [
    source({ type: "git", repository: "acme/web", rootDir: "/" }, { type: "git", repository: "acme/web", rootDir: "apps/web" }),
    source({ type: "image", image: "web:2", credentials: false }, { type: "image", image: "web:2", credentials: true }),
  ] }] }, services);
  expect(web?.rows.map((row) => [row.label, row.currentValue, row.newValue])).toEqual([
    ["Source", "acme/web", "acme/web in apps/web"],
    ["Source", "web:2", "web:2 with credentials"],
  ]);
});

it("words a Volume's storage by its limit", () => {
  expect(shownValue({ kind: "provisioned", maximumBytes: 4_100_000_000 })).toBe("4.1 GB limit");
  expect(shownValue({ kind: "docker" })).toBe("Docker volume");
});

it("reads a pending node as its Deployment does, and a vanished runner's node as Unknown, never Failed", () => {
  expect(nodeLight("pending", { status: "queued", in_flight: true })).toBe("queued");
  expect(nodeLight("pending", { status: "running", in_flight: true })).toBe("deploying");
  expect(nodeLight("pending", { status: "cancelled", in_flight: false })).toBe("not_applied");
  expect(nodeLight("deployed", { status: "failed", in_flight: false })).toBe("deployed");
  expect(nodeLight("unchanged", { status: "failed", in_flight: false })).toBe("deployed");
  expect(nodeLight("failed", { status: "failed", in_flight: false })).toBe("failed");
  expect(nodeLight("not_attempted", { status: "failed", in_flight: false })).toBe("not_applied");
  expect(nodeLight("unknown", { status: "unknown", in_flight: false })).toBe("unknown");
});

it("shows one action: retry after a Deployment ended without applying, start while queued, cancel before it ends", () => {
  expect(deploymentActions("failed", false)).toEqual({ primary: "retry", cancelInMenu: false });
  expect(deploymentActions("queued", false)).toEqual({ primary: "start", cancelInMenu: true });
  expect(deploymentActions("running", false)).toEqual({ primary: "cancel", cancelInMenu: false });
  expect(deploymentActions("cancelling", false)).toEqual({ primary: null, cancelInMenu: false });
  expect(deploymentActions("applied", false)).toEqual({ primary: null, cancelInMenu: false });
  // With no Server nothing can run it, so adding one comes first; a running one can still be cancelled.
  expect(deploymentActions("failed", true)).toEqual({ primary: "add_server", cancelInMenu: false });
  expect(deploymentActions("queued", true)).toEqual({ primary: "add_server", cancelInMenu: true });
  expect(deploymentActions("running", true)).toEqual({ primary: "cancel", cancelInMenu: false });
});

it("names an upload's provenance, and its uploader only when someone else started the Deployment", () => {
  expect(uploadLabel({ digest: "d", base: { commit: "abc1234def", changed: true }, uploader: "nick" })).toBe("Uploaded by nick · abc1234 + changes");
  expect(uploadLabel({ digest: "d", base: { commit: "abc1234def", changed: false }, uploader: "nick" }, "nick")).toBe("Uploaded · abc1234");
  expect(uploadLabel({ digest: "d", base: null })).toBe("Uploaded");
});

const node = (name: string, outcome: DeploymentView["nodes"][number]["outcome"]) => ({ type: "service" as const, id: `id-${name}`, name, outcome });
const deployment = (fields: Partial<DeploymentView>) => asTestDouble<DeploymentView>()({
  status: "failed", nodes: [], builds: [], upload: null, admitted_by: null, started_at: 100, outcome: { type: "executed", summary: null, reason: "x" },
  ...fields,
});

it("says who started a Deployment and what it ships: the Service's commit, else the first pinned one, else its upload", () => {
  const builds = [{ service: "web", commit: "8a7ed6e9f0", status: "built", message: null }, { service: "api", commit: "1234567abc", status: "built", message: null }] as const;
  expect(deploymentByline(deployment({ admitted_by: "Nick", builds: [...builds] }), "api")).toBe("by Nick · 1234567");
  expect(deploymentByline(deployment({ admitted_by: "Nick", builds: [...builds] }), "whoami")).toBe("by Nick · 8a7ed6e");
  // A retry of someone else's upload names both; one's own upload names the uploader once.
  const upload = { digest: "d", base: { commit: "abc1234def", changed: true }, uploader: "nick" };
  expect(deploymentByline(deployment({ admitted_by: "Ada", upload }), undefined)).toBe("by Ada · Uploaded by nick · abc1234 + changes");
  expect(deploymentByline(deployment({ admitted_by: "nick", upload }), undefined)).toBe("by nick · Uploaded · abc1234 + changes");
  expect(deploymentByline(deployment({}), undefined)).toBe("");
});

it("opens on the picked Service, else the failed one, else one it didn't apply, and offers a fix only for what ran and failed", () => {
  const nodes = [node("web", "deployed"), node("api", "not_attempted"), node("worker", "failed")];
  expect(focusedService(deployment({ nodes }), "id-web")?.name).toBe("web");
  expect(focusedService(deployment({ nodes }), undefined)?.name).toBe("worker");
  expect(focusedService(deployment({ nodes: nodes.slice(0, 2) }), undefined)?.name).toBe("api");
  expect(canFixOnBranch(deployment({ nodes }), node("worker", "failed"), false)).toBe(true);
  expect(canFixOnBranch(deployment({ nodes }), node("web", "deployed"), false)).toBe(false);
  // With no Server the way on is adding one; with nothing run, giving it a source.
  expect(canFixOnBranch(deployment({ nodes }), node("worker", "failed"), true)).toBe(false);
  expect(canFixOnBranch(deployment({ outcome: { type: "not_executed", reason: "x", needs_upload: [] } }), node("api", "pending"), false)).toBe(false);
});

it("says why a Service has no deploy logs: the Deployment never ran, or its turn never came", () => {
  expect(missingDeployLogs(deployment({ status: "queued", started_at: null, outcome: null }), node("web", "pending"))).toBe("Not started");
  expect(missingDeployLogs(deployment({ outcome: { type: "not_executed", reason: "x", needs_upload: [] } }), node("web", "pending"))).toBe("Not started");
  expect(missingDeployLogs(deployment({}), node("api", "not_attempted"))).toBe("Not attempted");
  expect(missingDeployLogs(deployment({}), node("cron", "removed"))).toBe("Removed");
  expect(missingDeployLogs(deployment({}), node("web", "failed"))).toBeNull();
  expect(missingDeployLogs(deployment({ started_at: null, outcome: { type: "forgotten" } }), node("web", "unknown"))).toBe("Nothing ran on a server");
});


it("words a node's outcome as the glossary does once it has one, and a pending one as its Deployment reads", () => {
  expect(nodeOutcomeLabel("pending", { status: "queued", in_flight: true })).toBe("Queued");
  expect(nodeOutcomeLabel("pending", { status: "running", in_flight: true })).toBe("Deploying");
  expect(nodeOutcomeLabel("pending", { status: "cancelled", in_flight: false })).toBe("Not attempted");
  expect(nodeOutcomeLabel("unchanged", { status: "applied", in_flight: false })).toBe("Unchanged");
  expect(nodeOutcomeLabel("not_attempted", { status: "failed", in_flight: false })).toBe("Not attempted");
});

it("words a Deployment that takes its Environment off the Servers as the Branch panel does, and ships nothing", () => {
  expect(deploymentStatusLabel({ status: "running", remove: true, outcome: null })).toBe("Coming off the servers");
  expect(deploymentStatusLabel({ status: "applied", remove: true, outcome: null })).toBe("Off the servers");
  expect(deploymentStatusLabel({ status: "failed", remove: true, outcome: null })).toBe("Failed");
  expect(deploymentStatusLabel({ status: "applied", remove: false, outcome: null })).toBe("Deployed");
  // No Server was left to take it off: it says so, and why.
  expect(deploymentStatusLabel({ status: "applied", remove: true, outcome: { type: "forgotten" } })).toBe("Left on old servers");
  expect(outcomeReason({ type: "forgotten" })).toContain("still there");
  expect(deploymentStatusLabel({ status: "applied", remove: true, outcome: { type: "never_ran" } })).toBe("Off the servers");
  expect(deploysLabel({ services: [], remove: true })).toBeNull();
  expect(deploysLabel({ services: ["web", "api"], remove: false })).toBe("Deploys web, api");
});
