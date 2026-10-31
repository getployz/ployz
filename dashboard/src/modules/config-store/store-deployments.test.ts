import { readFileSync } from "node:fs";
import type { DeploymentView, DiffView, JsonValue, MachineId, NodeOutcome, Outcome, RowId, ServiceId, ServiceListing } from "@ployz/sdk";
import { expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import {
  showCancelling,
  canFixOnBranch, changeGroups, deploymentWarningText, deploymentActions, deploymentByline, deploymentLight, deploymentStatusLabel, deploysLabel, missingDeployLogs, outcomeReason,
  deploymentLabel, freshRollback, nodeLight, nodeStatusLabels, reviewGroups, rollbackConfirm, shippedService, shownNodeStatus, shownValue,
  uploadLabel, warnedService,
} from "./store-deployments";

// The grouping reads only the changes and each Service's id and source.
const diff = asTestDouble<DiffView>()({
  total_count: 3,
  changes: [
    {
      type: "service", id: "s1", name: "web", lifecycle: "update", comparison: "head", data: null, restarts: [],
      settings: [
        { path: "web.replicas", kind: "update", before: 1, after: 2, canRestore: true, row: null },
        { path: "web.env.TOKEN", kind: "add", before: null, after: "abc", canRestore: true, row: null },
        { path: "web.mounts.pg-data", kind: "remove", before: "/data", after: null, canRestore: true, row: null },
      ],
    },
    { type: "volume", id: "v1", row: "v1:node" as RowId, name: "pg-data", lifecycle: "create", comparison: null, data: null, restarts: [], settings: [] },
  ],
});
const services = [asTestDouble<ServiceListing>()({ id: "s1", source: "image" })];

it("keeps atomic Config files, literal paths and restart facts in the review", () => {
  const before = { content: "PORT=80\nTOKEN=${{ web.TOKEN }}\n", mode: "0444", uid: 0, gid: 0 };
  const after = { ...before, mode: "0555", uid: 1000, gid: 1001 };
  const rows: DiffView["changes"][number]["settings"] = [
    { path: "configs.@c.files.nested/app.conf", kind: "update", before, after, canRestore: true, row: "c:files.nested/app.conf" as RowId },
    { path: "configs.@c.files.empty.conf", kind: "add", before: null, after: { ...before, content: "" }, canRestore: false, row: null },
    { path: "configs.@c.files.old.conf", kind: "remove", before, after: null, canRestore: true, row: null },
  ];
  const restarts = ["web", "worker"];
  const [config, web] = changeGroups({ ...diff, changes: [
    { type: "config", id: "c", row: "c:node" as RowId, name: "sentry", lifecycle: "update", comparison: "head", data: null, restarts, settings: rows },
    { type: "service", id: "s1", row: "s1:node" as RowId, name: "web", lifecycle: "update", comparison: "head", data: null, restarts: [],
      settings: [{ path: "web.configs.@c", configName: "sentry", kind: "add", before: null, after: "/etc/sentry", canRestore: true, row: null }] },
  ] }, services);
  expect(config).toMatchObject({ discardPath: "configs.@c", changeCount: 3, restarts });
  expect(config?.restarts).toBe(restarts);
  expect(config?.rows.map((row) => [row.path, row.label, row.currentValue, row.newValue, row.canDiscard])).toEqual([
    [rows[0]?.path, "nested/app.conf", "File", "File", true],
    [rows[1]?.path, "empty.conf", "", "File", false],
    [rows[2]?.path, "old.conf", "File", "", true],
  ]);
  expect(config?.rows[0]).toMatchObject({ row: rows[0]?.row, configFile: { before, after } });
  expect(config?.rows[1]?.configFile).toEqual({ before: null, after: { ...before, content: "" } });
  expect(config?.rows[2]?.configFile).toEqual({ before, after: null });
  expect(web?.rows[0]?.label).toBe("Config mount sentry");
});

const malformedFiles: JsonValue[] = [{ content: "private" }, "private", { content: "private", mode: "0444", uid: "0", gid: 0 }];
it.each(malformedFiles)(
  "refuses a malformed non-null Config file without exposing its value", (value) => {
    const adapt = () => changeGroups({ ...diff, changes: [{
      type: "config", id: "c", row: "c:node" as RowId, name: "sentry", lifecycle: "update", comparison: null, data: null, restarts: [],
      settings: [{ path: "configs.@c.files.app.conf", kind: "update", before: null, after: value, canRestore: true, row: null }],
    }] }, []);
    expect(adapt).toThrow("Could not read Config file comparison for configs.@c.files.app.conf.");
    expect(adapt).not.toThrow(/private/);
  },
);

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
  const [volume] = changeGroups({ ...diff, changes: [{ type: "volume", id: "v1", row: "v1:node" as RowId, name: "store", lifecycle: "update", comparison: null, data: null, restarts: [], settings: [
    { path: "volumes.store.name", kind: "update", before: "pg-data", after: "store", canRestore: true, row: null },
    { path: "volumes.store.storage", kind: "update", before: null, after: { kind: "docker" }, canRestore: false, row: null },
  ] }] }, services);
  expect(volume?.rows.map((row) => [row.path, row.canDiscard])).toEqual([["volumes.store.name", true], ["volumes.store.storage", false]]);
});

it("words a branch change as the branch, not its stored object", () => {
  const [web] = changeGroups({ ...diff, changes: [{ type: "service", id: "s1", row: "s1:node" as RowId, name: "web", lifecycle: "update", comparison: "head", data: null, restarts: [], settings: [
    { path: "web.source.branch", kind: "update", before: { type: "connected", name: "main" }, after: { type: "connected", name: "release" }, canRestore: true, row: null },
  ] }] }, services);
  expect(web?.rows.map((row) => [row.label, row.currentValue, row.newValue])).toEqual([["Branch", "main", "release"]]);
});

it("words a source change readably, repeating only the tag when that is all that moved", () => {
  // A row's cell, as core's `source_cell` writes it: credentials by presence, the branch on its own row.
  const source = (before: JsonValue, after: JsonValue) => ({ path: "web.source", kind: "update" as const, before, after, canRestore: true, row: null });
  const [web] = changeGroups({ ...diff, changes: [{ type: "service", id: "s1", row: "s1:node" as RowId, name: "web", lifecycle: "update", comparison: "head", data: null, restarts: [], settings: [
    source({ type: "git", repository: "acme/web", rootDir: "/" }, { type: "git", repository: "acme/web", rootDir: "apps/web" }),
    source({ type: "image", image: "web:2", credentials: false }, { type: "image", image: "web:2", credentials: true }),
    source({ type: "image", image: "ghcr.io/acme/api:1.4", credentials: false }, { type: "image", image: "ghcr.io/acme/api:1.5", credentials: false }),
  ] }] }, services);
  expect(web?.rows.map((row) => [row.label, row.currentValue, row.newValue])).toEqual([
    ["Source", "acme/web", "acme/web in apps/web"],
    ["Source", "web:2", "web:2 with credentials"],
    ["Source", "ghcr.io/acme/api:1.4", "1.5"],
  ]);
});

it("words a healthcheck by its path or command and timeout", () => {
  expect(shownValue({ path: "/up", timeoutSeconds: 30 })).toBe("/up within 30s");
  expect(shownValue({ command: "pg_isready -h 127.0.0.1", timeoutSeconds: 300 })).toBe("pg_isready -h 127.0.0.1 within 300s");
});

it("words a secret as Sealed, or as needing a value until it has one", () => {
  expect(shownValue({ secret: true })).toBe("Sealed");
  expect(shownValue({ secret: false })).toBe("Not set");
});

it("words a memory limit in the unit it was likely given", () => {
  const [web] = changeGroups({ ...diff, changes: [{ type: "service", id: "s1", row: "s1:node" as RowId, name: "web", lifecycle: "update", comparison: "head", data: null, restarts: [], settings: [
    { path: "web.memLimit", kind: "update", before: 0.201326592, after: 0.5, canRestore: true, row: null },
    { path: "web.memLimit", kind: "add", before: null, after: 1.073741824, canRestore: true, row: null },
  ] }] }, services);
  expect(web?.rows.map((row) => [row.label, row.currentValue, row.newValue])).toEqual([
    ["Memory limit", "192 MiB", "0.5 GB"],
    ["Memory limit", "", "1 GiB"],
  ]);
});

it("words a CPU limit in vCPUs", () => {
  const [web] = changeGroups({ ...diff, changes: [{ type: "service", id: "s1", row: "s1:node" as RowId, name: "web", lifecycle: "update", comparison: "head", data: null, restarts: [], settings: [
    { path: "web.cpuLimit", kind: "update", before: 0.5, after: 2, canRestore: true, row: null },
    { path: "web.cpuLimit", kind: "add", before: null, after: 1, canRestore: true, row: null },
  ] }] }, services);
  expect(web?.rows.map((row) => [row.currentValue, row.newValue])).toEqual([
    ["0.5 vCPUs", "2 vCPUs"],
    ["", "1 vCPU"],
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

it("shows one action: redeploy once a Deployment ended, start while queued, cancel before it ends", () => {
  expect(deploymentActions({ status: "failed", remove: false, held: false }, false)).toEqual({ primary: "redeploy", cancelInMenu: false });
  expect(deploymentActions({ status: "queued", remove: false, held: false }, false)).toEqual({ primary: "start", cancelInMenu: true });
  expect(deploymentActions({ status: "running", remove: false, held: false }, false)).toEqual({ primary: "cancel", cancelInMenu: false });
  expect(deploymentActions({ status: "cancelling", remove: false, held: false }, false)).toEqual({ primary: null, cancelInMenu: false });
  expect(deploymentActions({ status: "applied", remove: false, held: false }, false)).toEqual({ primary: "redeploy", cancelInMenu: false });
  expect(deploymentActions({ status: "superseded", remove: false, held: false }, false)).toEqual({ primary: null, cancelInMenu: false });
  // With no Server nothing can run it, so adding one comes first; a running one can still be cancelled.
  expect(deploymentActions({ status: "failed", remove: false, held: false }, true)).toEqual({ primary: "add_server", cancelInMenu: false });
  expect(deploymentActions({ status: "queued", remove: false, held: false }, true)).toEqual({ primary: "add_server", cancelInMenu: true });
  expect(deploymentActions({ status: "running", remove: false, held: false }, true)).toEqual({ primary: "cancel", cancelInMenu: false });
  // One waiting for CI queues once its CI passes; only Cancel is left to do.
  expect(deploymentActions({ status: "waiting_for_ci", remove: false, held: true }, false)).toEqual({ primary: "cancel", cancelInMenu: false });
  expect(deploymentActions({ status: "waiting_for_ci", remove: false, held: true }, true)).toEqual({ primary: "cancel", cancelInMenu: false });
  // Cancelled while it waited, it froze nothing to redeploy, Server or not.
  expect(deploymentActions({ status: "cancelled", remove: false, held: false }, false)).toEqual({ primary: "redeploy", cancelInMenu: false });
  expect(deploymentActions({ status: "cancelled", remove: false, held: true }, false)).toEqual({ primary: null, cancelInMenu: false });
  expect(deploymentActions({ status: "cancelled", remove: false, held: true }, true)).toEqual({ primary: null, cancelInMenu: false });
  // A removal that applied left nothing to redeploy; one that failed redeploys.
  expect(deploymentActions({ status: "applied", remove: true, held: false }, false)).toEqual({ primary: null, cancelInMenu: false });
  expect(deploymentActions({ status: "failed", remove: true, held: false }, false)).toEqual({ primary: "redeploy", cancelInMenu: false });
});

it("names a Deployment by number once it queued, and by its push's short sha while it waits for CI", () => {
  expect(deploymentLabel({ number: 4, id: "dep_0123456789", push: { head: "abcdef0123456789" } })).toBe("#4");
  expect(deploymentLabel({ number: null, id: "dep_0123456789", push: { head: "abcdef0123456789" } })).toBe("abcdef0");
  expect(deploymentLabel({ number: null, id: "dep_0123456789" })).toBe("dep_0123");
});

it("asks before a redeploy that rolls back Deployments that ran after it, and only then", () => {
  expect(rollbackConfirm({ number: 6, rolls_back: [] })).toBeNull();
  expect(rollbackConfirm({ number: 4, rolls_back: [5] })).toEqual({ title: "Roll back to #4", body: "Undoes #5" });
  expect(rollbackConfirm({ number: 4, rolls_back: [5, 6] })).toEqual({ title: "Roll back to #4", body: "Undoes #5 and #6" });
  expect(rollbackConfirm({ number: 3, rolls_back: [5, 6, 8] })).toEqual({ title: "Roll back to #3", body: "Undoes #5, #6 and #8" });
});

it("reads what a redeploy rolls back now from the Store's refusal, and nothing from any other", () => {
  expect(freshRollback({ code: "confirmation_required", details: { deployment: "d", number: 4, rolls_back: [5, 6] } })).toEqual([5, 6]);
  expect(freshRollback({ code: "confirmation_required", details: { rolls_back: [] } })).toEqual([]);
  expect(freshRollback({ code: "confirmation_required", details: { volumes: [] } })).toBeNull();
  expect(freshRollback({ code: "conflict", details: { rolls_back: [5] } })).toBeNull();
});

it("words a push waiting for CI by how its CI stands, and lights it red once its CI failed", () => {
  const waiting = { status: "waiting_for_ci", remove: false, outcome: null } as const;
  expect(deploymentStatusLabel({ ...waiting, ci: "pending" })).toBe("Waiting for CI");
  expect(deploymentStatusLabel({ ...waiting, ci: "failed" })).toBe("CI failed");
  expect(deploymentLight({ status: "waiting_for_ci", ci: "pending" })).toBe("queued");
  expect(deploymentLight({ status: "waiting_for_ci", ci: "failed" })).toBe("failed");
  expect(deploymentLight({ status: "running", ci: null })).toBe("deploying");
});

it("names an upload's provenance, and its uploader only when someone else started the Deployment", () => {
  expect(uploadLabel({ upload: { digest: "d", base: { commit: "abc1234def", changed: true }, uploader: "nick" } })).toBe("Uploaded by nick · abc1234 + changes");
  expect(uploadLabel({ upload: { digest: "d", base: { commit: "abc1234def", changed: false }, uploader: "nick" } }, "nick")).toBe("Uploaded · abc1234");
  expect(uploadLabel({ upload: { digest: "d", base: null } })).toBe("Uploaded");
  expect(uploadLabel({ upload: null })).toBeNull();
});

it("says a waiting Deploy's upload is chosen for now, and again when it starts", () => {
  const upload = { digest: "d", base: { commit: "abc1234def", changed: false }, uploader: "nick" };
  expect(uploadLabel({ upload, chooses_upload_at_start: true })).toBe("Uploaded by nick · abc1234 for now; chosen again when it starts");
  expect(uploadLabel({ upload, chooses_upload_at_start: false })).toBe("Uploaded by nick · abc1234");
});

it("reads a running Deployment as preparing until its runner records a Deploy Preview", () => {
  const running = { status: "running", remove: false, outcome: null } as const;
  expect(deploymentStatusLabel({ ...running, preparing: true })).toBe("Preparing");
  expect(deploymentStatusLabel(running)).toBe("Deploying");
  expect(deploymentStatusLabel({ ...running, remove: true, preparing: true })).toBe("Coming off the servers");
  expect(deploymentLight({ status: "running" })).toBe("deploying");
});

const node = (name: string, outcome: DeploymentView["nodes"][number]["outcome"]) => ({ type: "service" as const, id: `id-${name}`, name, outcome, rows: [] });
const deployment = (fields: Partial<DeploymentView>) => asTestDouble<DeploymentView>()({
  status: "failed", nodes: [], builds: [], upload: null, admitted_by: null, started_at: 100, outcome: { type: "executed", summary: null, reason: "x" },
  ...fields,
});

it("says who started a Deployment and what it ships: the Service's commit, else the first pinned one, else its upload", () => {
  const builds = [{ service: "web", commit: "8a7ed6e9f0", status: "built", message: null, started_at: null, finished_at: null, server: null, github_skipped: null }, { service: "api", commit: "1234567abc", status: "built", message: null, started_at: null, finished_at: null, server: null, github_skipped: null }] as const;
  expect(deploymentByline(deployment({ admitted_by: "Nick", builds: [...builds] }), "api")).toBe("by Nick · 1234567");
  expect(deploymentByline(deployment({ admitted_by: "Nick", builds: [...builds] }), "whoami")).toBe("by Nick · 8a7ed6e");
  // A retry of someone else's upload names both; one's own upload names the uploader once.
  const upload = { digest: "d", base: { commit: "abc1234def", changed: true }, uploader: "nick" };
  expect(deploymentByline(deployment({ admitted_by: "Ada", upload }), undefined)).toBe("by Ada · Uploaded by nick · abc1234 + changes");
  expect(deploymentByline(deployment({ admitted_by: "nick", upload }), undefined)).toBe("by nick · Uploaded · abc1234 + changes");
  expect(deploymentByline(deployment({}), undefined)).toBe("");
});

it("offers a fix only for what ran and failed", () => {
  const nodes = [node("web", "deployed"), node("api", "not_attempted"), node("worker", "failed")];
  expect(canFixOnBranch(deployment({ nodes }), node("worker", "failed"), false)).toBe(true);
  expect(canFixOnBranch(deployment({ nodes }), node("web", "deployed"), false)).toBe(false);
  // With no Server the way on is adding one; with nothing run, giving it a source.
  expect(canFixOnBranch(deployment({ nodes }), node("worker", "failed"), true)).toBe(false);
  expect(canFixOnBranch(deployment({ outcome: { type: "not_executed", reason: "x", cause: [], needs_upload: [] } }), node("api", "pending"), false)).toBe(false);
});

it("says why a Service has no deploy logs: the Deployment never ran, or its turn never came", () => {
  expect(missingDeployLogs(deployment({ status: "queued", started_at: null, outcome: null }), node("web", "pending"))).toBe("Not started");
  expect(missingDeployLogs(deployment({ outcome: { type: "not_executed", reason: "x", cause: [], needs_upload: [] } }), node("web", "pending"))).toBe("Not started");
  expect(missingDeployLogs(deployment({}), node("api", "not_attempted"))).toBe("Not attempted");
  expect(missingDeployLogs(deployment({}), node("cron", "removed"))).toBe("Removed");
  expect(missingDeployLogs(deployment({}), node("web", "failed"))).toBeNull();
  expect(missingDeployLogs(deployment({ started_at: null, outcome: { type: "forgotten" } }), node("web", "unknown"))).toBe("Nothing ran on a server");
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
  expect(deploysLabel({ services: [], ships: ["web", "api"], remove: true })).toBeNull();
});

it("names a Deploy's targets in their order, then the Services it ships by reference alphabetically", () => {
  expect(deploysLabel({ services: ["web"], ships: ["web", "postgres", "api"], remove: false })).toBe("Deploys web (+ api, postgres)");
  expect(deploysLabel({ services: ["web"], ships: ["web"], remove: false })).toBe("Deploys web");
  expect(deploysLabel({ services: ["web", "api"], ships: ["api", "web", "redis"], remove: false })).toBe("Deploys web, api (+ redis)");
  expect(deploysLabel({ services: [], ships: ["web", "api", "postgres"], remove: false })).toBe("Deploys every service");
  // Waiting for CI, nothing is frozen yet: it names only its targets.
  expect(deploysLabel({ services: ["web"], ships: [], remove: false })).toBe("Deploys web");
  // A rollback or retry reads its own ships, so it lists what it ships now, not what its source shipped.
  const source = { services: ["web"], ships: ["web", "api", "postgres"], remove: false };
  expect(deploysLabel({ ...source, ships: ["web", "api"] })).toBe("Deploys web (+ api)");
});


it("keeps the deepest execution failure cause in the Deployment banner", () => {
  const outcome: Outcome = {
    type: "executed", summary: null, reason: "remove Container failed",
    cause: ["the daemon is busy", "connection refused"], services: [],
  };
  expect(outcomeReason(outcome)).toBe("remove Container failed: connection refused");
  expect(outcome.cause).toEqual(["the daemon is busy", "connection refused"]);
});

it("keeps the deepest preparation failure cause in the Deployment banner", () => {
  const outcome: Outcome = {
    type: "not_executed", reason: "Could not prepare the deployment", needs_upload: [],
    cause: ["could not pull the image", "registry denied access"],
  };
  expect(outcomeReason(outcome)).toBe("Could not prepare the deployment: registry denied access");
  expect(outcome.cause).toEqual(["could not pull the image", "registry denied access"]);
});

it("closes a reason in whole sentences with its cause, and never reads inside the reason", () => {
  const outcome: Outcome = {
    type: "not_executed", needs_upload: [],
    reason: "Server machine-1 isn't answering. Check that its Ployz daemon is running, then retry.",
    cause: ["management connection timed out"],
  };
  const read = (reason: string, cause: string) => outcomeReason({ ...outcome, reason, cause: [cause] });
  expect(outcomeReason(outcome))
    .toBe("Server machine-1 isn't answering. Check that its Ployz daemon is running, then retry. Cause: management connection timed out.");
  // The cause belongs to the Server tried last, so it follows the whole reason, not its first sentence.
  expect(read(
    "Ployz couldn't reach a Server's daemon through any of the 2 connections from Cloud. Server web-2, tried last, isn't answering. Check that its Ployz daemon is running, then retry.",
    "connection refused",
  )).toBe("Ployz couldn't reach a Server's daemon through any of the 2 connections from Cloud. Server web-2, tried last, isn't answering. Check that its Ployz daemon is running, then retry. Cause: connection refused.");
  expect(read("Ployz couldn't reach your Server's daemon.", "Timed out after 10s."))
    .toBe("Ployz couldn't reach your Server's daemon. Cause: Timed out after 10s.");
  // An image tag may end in a dot; a reason that isn't a sentence keeps it whole.
  expect(read("Image ghcr.io/acme/web:release-2026.10. not found, or its registry needs credentials", "manifest unknown"))
    .toBe("Image ghcr.io/acme/web:release-2026.10. not found, or its registry needs credentials: manifest unknown");
  expect(read("web: Cancelled", "registry denied access")).toBe("web: Cancelled: registry denied access");
});

it("drops the header's reason when the Deployment's cancel stopped the work it names, and keeps a real failure's", () => {
  const stopped: Outcome = { type: "executed", summary: null, reason: "web: Cancelled", cause: [], services: [] };
  expect(outcomeReason({ ...stopped, cancelled: true })).toBeUndefined();
  // A Store from before the marker sends none, so the line reads as it did.
  expect(outcomeReason(stopped)).toBe("web: Cancelled");
  expect(outcomeReason({ ...stopped, reason: "api: create Container failed", cause: ["no space left on device"] }))
    .toBe("api: create Container failed: no space left on device");
});

it("names a real failure in the header beside the work a cancel stopped, as the Store records the run", () => {
  // ployz-store's deployment test writes this snapshot: a cancel stops web while api's create fails on its own.
  const snapshot = new URL("../../../../core/crates/ployz-store/tests/snapshots/cancel_beside_a_real_failure.json", import.meta.url);
  const recorded = asTestDouble<{ outcome: Outcome; nodes: (Pick<NodeOutcome, "outcome" | "cancelled"> & { name: string })[] }>()<unknown>(
    JSON.parse(readFileSync(snapshot, "utf8")),
  );
  expect(outcomeReason(recorded.outcome)).toBe("api: create Container failed: no space left on device");
  expect(Object.fromEntries(recorded.nodes.map((node) => [node.name, nodeStatusLabels[shownNodeStatus(node)]])))
    .toEqual({ api: "Failed", web: "Cancelled" });
});

it("preserves reasons without a cause and outcomes without a reason", () => {
  expect(outcomeReason(null)).toBeUndefined();
  expect(outcomeReason({ type: "never_ran" })).toBeUndefined();
  expect(outcomeReason({ type: "executed", summary: null, reason: null, cause: ["connection refused"], services: [] })).toBeUndefined();
  expect(outcomeReason({ type: "executed", summary: null, reason: "", cause: [], services: [] })).toBe("");
  expect(outcomeReason({ type: "executed", summary: null, reason: "Cancelled", cause: [], services: [] })).toBe("Cancelled");
  expect(outcomeReason({ type: "not_executed", reason: "No source", needs_upload: [], cause: [] })).toBe("No source");
  expect(outcomeReason(asTestDouble<Outcome>()({ type: "executed", summary: null, reason: "Old execution failure" }))).toBe("Old execution failure");
  expect(outcomeReason(asTestDouble<Outcome>()({ type: "not_executed", reason: "Old preparation failure", needs_upload: [] }))).toBe("Old preparation failure");
});


it("gives same-name Configs separate whole and file discard targets", () => {
  const file = { content: "draft", mode: "0444", uid: 0, gid: 0 };
  const groups = changeGroups({ ...diff, changes: ["old", "new"].map((id) => ({ type: "config", id, row: `${id}:node` as RowId,
    name: "sentry", lifecycle: "update", comparison: null, data: null, restarts: [], settings: [
      { path: `configs.@${id}.files.app.conf`, kind: "update", before: file, after: file, canRestore: true, row: null },
    ] })) }, []);
  expect(groups.map((group) => [group.discardPath, group.rows[0]?.path, group.rows[0]?.label])).toEqual([
    ["configs.@old", "configs.@old.files.app.conf", "app.conf"],
    ["configs.@new", "configs.@new.files.app.conf", "app.conf"],
  ]);
});


it("retains inverse draft fields, with their Saved discard target, in one runtime review", () => {
  const runtime = asTestDouble<DiffView["changes"][number]>()(diff.changes[0] ?? {});
  const groups = reviewGroups({ ...diff, changes: [runtime], draft_changes: [{ ...runtime, comparison: "saved", settings: [
    ...runtime.settings.slice(0, 1),
    { path: "web.startCommand", kind: "update", before: "new", after: null, canRestore: true, row: "s1:startCommand" as RowId },
  ] }] }, services);
  expect(groups).toHaveLength(1);
  expect(groups[0]?.changeCount).toBe(4);
  expect(groups[0]?.rows.map((row) => [row.path, row.discardTarget])).toEqual([
    ["web.replicas", undefined], ["web.env.TOKEN", undefined], ["web.mounts.pg-data", undefined], ["web.startCommand", "saved"],
  ]);
});

it("keeps separate value, export and route identities when their Sync row is shared", () => {
  const runtime = { ...asTestDouble<DiffView["changes"][number]>()(diff.changes[0] ?? {}), settings: [
    { path: "web.env.KEY", kind: "update" as const, before: "old", after: "new", canRestore: true, row: "s1:variables.KEY" as RowId },
    { path: "web.routes.first", kind: "add" as const, before: null, after: { id: "first", hostname: "first.example.com" }, canRestore: false, row: "s1:routes" as RowId },
  ] };
  const [group] = reviewGroups({ ...diff, changes: [runtime], draft_changes: [{ ...runtime, comparison: "saved", settings: [
    ...runtime.settings.slice(0, 1),
    { path: "web.env.KEY.exported", kind: "update", before: false, after: true, canRestore: true, row: "s1:variables.KEY" as RowId },
    { path: "web.env.KEY.description", kind: "update", before: null, after: "For web", canRestore: true, row: "s1:variables.KEY" as RowId },
    { path: "web.routes.second", kind: "add", before: null, after: { id: "second", hostname: "second.example.com" }, canRestore: false, row: "s1:routes" as RowId },
  ] }] }, services);
  expect(group?.rows.map((row) => row.path)).toEqual([
    "web.env.KEY", "web.routes.first", "web.env.KEY.exported", "web.env.KEY.description", "web.routes.second",
  ]);
  expect(group?.changeCount).toBe(5);
});

it("marks a node the Working State has back since the Saved version as coming back, and a new one as not", () => {
  const head = asTestDouble<DiffView["changes"][number]>()(diff.changes[0] ?? {});
  const restored = { ...head, lifecycle: "create" as const, comparison: "saved" as const, settings: [] };
  const [back] = reviewGroups({ ...diff, changes: [], draft_changes: [restored] }, services);
  expect(back?.lifecycle).toBe("create");
  expect(back?.comesBack).toBe(true);

  const added = { ...head, lifecycle: "create" as const, settings: [] };
  const [created] = reviewGroups({ ...diff, changes: [added], draft_changes: [restored] }, services);
  expect(created?.comesBack).toBeUndefined();

  const changed = { ...head, lifecycle: "update" as const };
  const [merged] = reviewGroups({ ...diff, changes: [changed], draft_changes: [restored] }, services);
  expect(merged?.lifecycle).toBe("create");
  expect(merged?.comesBack).toBe(true);
});

it("lists a draft-only node by kind and name among the runtime changes, as the CLI does", () => {
  const web = asTestDouble<DiffView["changes"][number]>()(diff.changes[0] ?? {});
  const volume = asTestDouble<DiffView["changes"][number]>()(diff.changes[1] ?? {});
  const api = { ...web, id: "s2", name: "api", comparison: "saved" as const, settings: [] };
  const old = { ...web, id: "s0", lifecycle: "delete" as const, comparison: "saved" as const, settings: [] };
  const groups = reviewGroups({ ...diff, changes: [{ ...web, lifecycle: "create" }, volume], draft_changes: [api, old] }, services);
  expect(groups.map((group) => [group.nodeName, group.lifecycle])).toEqual([["api", "update"], ["web", "delete"], ["web", "create"], ["pg-data", "create"]]);
});

it("lets a whole runtime lifecycle dominate draft children and counts its node marker", () => {
  const runtime = { ...asTestDouble<DiffView["changes"][number]>()(diff.changes[0] ?? {}), lifecycle: "delete" as const };
  const [group] = reviewGroups({ ...diff, changes: [runtime], draft_changes: [{ ...runtime, lifecycle: "update", comparison: "saved", settings: [
    { path: "web.startCommand", kind: "update", before: null, after: "later", canRestore: true, row: null },
  ] }] }, services);
  expect(group?.changeCount).toBe(4);
  expect(group?.rows).toHaveLength(3);
  expect(group?.discardTarget).toBeUndefined();
});

it("words what a Deployment shipped despite", () => {
  expect(deploymentWarningText({ service: "web", variable: "TOKEN" })).toBe("TOKEN not set in web");
  expect(deploymentWarningText({ service: "web", variable: "VIA", reads: "web.TOKEN" })).toBe("VIA not set in web: it reads web.TOKEN, which is not set");
  expect(deploymentWarningText({ service: "app", variable: "DBURL", reads: "db.TOK3", sealed_elsewhere: true }))
    .toBe("DBURL not set in app: it reads db.TOK3, a sealed value this branch doesn't get from its parent. Set DBURL here, or make db separate and set TOK3 there, though its volumes start empty");
  expect(deploymentWarningText({ config: "sentry", file: "config.yml", secret: "web.TOKEN" })).toBe("web.TOKEN not set in sentry/config.yml");
  expect(deploymentWarningText({ config: "sentry", file: "config.yml", variable: "web.DSN" })).toBe("web.DSN may break sentry/config.yml");
  expect(deploymentWarningText({ service: "sentry", variable: "REDIS_URL", broken: true })).toBe("Broken reference: REDIS_URL in sentry");
  expect(deploymentWarningText({ config: "sentry", file: "config.yml", secret: "redis.URL", broken: true }))
    .toBe("Broken reference: redis.URL in sentry/config.yml");
  expect(deploymentWarningText({ server: "web-1", ingress: { type: "port_taken", cause: "address already in use" } }))
    .toBe("Ingress isn't running on web-1: something else holds port 80 or 443");
  expect(deploymentWarningText({ server: "web-1", ingress: { type: "failed", cause: "image pull failed" } }))
    .toBe("Ingress isn't running on web-1: image pull failed");
  expect(deploymentWarningText({ cause: "Couldn't list the Cluster's Servers." }))
    .toBe("Couldn't check ingress: Couldn't list the Cluster's Servers. Deploy again to retry");
  const mismatch = { warning: { type: "image_port_mismatch" as const, service: "web", service_id: "f".repeat(32) as ServiceId, image: "nginx", machines: ["a".repeat(32) as MachineId], exposed: [80], routed: [8080, 8081] } };
  expect(deploymentWarningText(mismatch)).toBe("web's image nginx exposes 80; its domains send to 8080, 8081");
  expect(deploymentWarningText(mismatch, "frontend")).toBe("frontend's image nginx exposes 80; its domains send to 8080, 8081");
});

it("links a port warning to the Service by its runtime name, so a rename keeps the link and a namesake never takes it", () => {
  const mismatch = { warning: { type: "image_port_mismatch" as const, service: "web", service_id: "f".repeat(32) as ServiceId, image: "nginx", machines: [], exposed: [80], routed: [8080] } };
  // The warned Service was web when it got its DNS name and is frontend now; another Service took the name web.
  const nodes = [node("frontend", "deployed"), node("web", "deployed")];
  const runtime_names = { frontend: "web", web: "web-2" };
  expect(warnedService({ nodes, runtime_names }, mismatch)?.id).toBe("id-frontend");
  // A Service that never changed name matches by its own name.
  expect(warnedService({ nodes: [node("web", "deployed")], runtime_names: { web: "web" } }, mismatch)?.id).toBe("id-web");
  expect(warnedService({ nodes: [node("api", "deployed")], runtime_names: { api: "api" } }, mismatch)).toBeUndefined();
  expect(warnedService({ nodes, runtime_names }, { service: "web", variable: "TOKEN" })).toBeUndefined();
});

it("shows a Cancel at once as Cancelling while the Deployment runs or waits, and leaves an ended one alone", () => {
  expect(showCancelling({ status: "running", in_flight: true }, true)).toEqual({ status: "cancelling", in_flight: true });
  expect(showCancelling({ status: "queued", in_flight: true }, false)).toEqual({ status: "queued", in_flight: true });
  expect(showCancelling({ status: "applied", in_flight: false }, true)).toEqual({ status: "applied", in_flight: false });
});

it("counts a Service as shipped by a full Deploy, one that names it, and one that ships it because a named one references it", () => {
  const narrowed = { services: ["web"], ships: ["web", "db"] };
  expect(shippedService(narrowed, "web")).toBe(true);
  expect(shippedService(narrowed, "db")).toBe(true);
  expect(shippedService(narrowed, "api")).toBe(false);
  expect(shippedService({ services: [], ships: ["web"] }, "api")).toBe(true);
  expect(shippedService({ services: ["web"], ships: [] }, "web")).toBe(true);
});
