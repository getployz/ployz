import { afterAll, beforeAll, expect, it } from "vitest";
import * as schema from "#/db/schema";
import { readCollection } from "#/collections/read.server";
import {
  createDefaultServiceHealthcheck, createDefaultServiceRestartPolicy, createImageServiceSource, projectServiceDeploymentConfig,
} from "#/modules/environment-design/services";
import { type GithubPostgresTestHarness, startGithubPostgresTestHarness } from "#/modules/github/github-ingestion.postgres-test-harness";
import type { DeploymentProgress, DeploymentProgressRow } from "./deployment-progress";
import { attemptTarget, deploymentView } from "./deployment-view";

const organizationId = "00000000-0000-4000-8000-000000000901";
const userId = "00000000-0000-4000-8000-000000000902";
const projectId = "00000000-0000-4000-8000-000000000903";
const environmentId = "00000000-0000-4000-8000-000000000904";
const [applied, target, later] = ["911", "912", "913"].map((suffix) => `00000000-0000-4000-8000-000000000${suffix}`) as [string, string, string];
const [api, removed, web, data, afterwards] = ["921", "922", "923", "924", "925"].map((suffix) => `00000000-0000-4000-8000-000000000${suffix}`) as [string, string, string, string, string];

const service = (privateDns: string, image = "nginx:1") => projectServiceDeploymentConfig({
  source: createImageServiceSource({ image }), preDeployCommand: null, startCommand: null,
  healthcheck: createDefaultServiceHealthcheck(), restartPolicy: createDefaultServiceRestartPolicy(), privateDns,
});
const row = (index: number, serviceId: string | null, serviceName: string, operation: string, status: DeploymentProgressRow["status"]): DeploymentProgressRow => ({
  index, machineId: "machine", machineName: "server", serviceId, runtimeServiceId: null, serviceName, displayName: null,
  operation, target: null, updateOrder: null, status, phase: null, elapsedMs: null, deadlineMs: null, health: null,
  error: status === "failed" ? "Health check timed out" : null, containerId: status === "failed" ? "c0ffee" : null, startedAt: null, finishedAt: null,
});
// The record side leaves the removed service's row without a serviceId: that attempt has no snapshot of it.
const progress: DeploymentProgress = { completed: 2, total: 3, outcome: "failed", compensation: [], rows: [
  row(0, null, "old", "remove_container", "completed"), row(1, api, "api", "replace_container", "completed"), row(2, web, "web", "run_container", "failed"),
] };

let harness: GithubPostgresTestHarness;
beforeAll(async () => { harness = await startGithubPostgresTestHarness(); }, 60_000);
afterAll(async () => { await harness?.stop(); });

it("builds an attempt's full node set from real deployment snapshots", async () => {
  await harness.pool.query(`
    insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
    insert into "user" (id, email, name) values ('${userId}', 'owner@example.com', 'Owner');
    insert into member (user_id, organization_id, role) values ('${userId}', '${organizationId}', 'owner');
    insert into project (id, organization_id, name, slug) values ('${projectId}', '${organizationId}', 'Shop', 'shop');
    insert into environment (id, project_id, organization_id, name, namespace, intent) values ('${environmentId}', '${projectId}', '${organizationId}',
      'Production', 'production', '{"version":1,"environmentSlug":"production","services":[],"volumes":[]}');
  `);
  const [saved] = await harness.db.insert(schema.environmentSavedStateSnapshot).values({
    organizationId, environmentId, actorId: userId, intent: { version: 1, environmentSlug: "production", services: [], volumes: [] }, volumeDeletionAuthorizations: [],
  }).returning();
  const deployment = (id: string, status: "applied" | "failed" | "queued", minute: number) => ({
    id, organizationId, environmentId, savedStateSnapshotId: saved?.id ?? "", status,
    triggerOrigin: { origin: "manual" as const, actorId: userId }, createdAt: new Date(Date.UTC(2026, 8, 1, 0, minute)),
    finishedAt: status === "queued" ? null : new Date(Date.UTC(2026, 8, 1, 0, minute)),
  });
  await harness.db.insert(schema.environmentDeployment).values([
    deployment(applied, "applied", 1), { ...deployment(target, "failed", 2), runtimeProgress: progress }, deployment(later, "queued", 3),
  ]);
  const snapshot = (deploymentId: string, nodeId: string, config: typeof schema.environmentNodeConfigSnapshot.$inferInsert["config"], nodeType: "service" | "volume" = "service") => ({
    organizationId, environmentId, environmentDeploymentId: deploymentId, nodeType, nodeId, nodeLineageId: nodeId, config,
  });
  await harness.db.insert(schema.environmentNodeConfigSnapshot).values([
    snapshot(applied, api, service("api")), snapshot(applied, removed, service("old")), snapshot(applied, data, { version: 2, name: "data" }, "volume"),
    snapshot(target, api, service("api", "nginx:2")), snapshot(target, web, service("web")), snapshot(target, data, { version: 2, name: "data" }, "volume"),
    snapshot(later, api, service("api", "nginx:2")), snapshot(later, afterwards, service("later")),
  ]);

  const read = async (table: "environment_deployment" | "environment_node_config_snapshot") =>
    (await harness.runEffect(readCollection({ userId }, { table, userId, organizationSlug: "acme" }))).rows;
  // SAFETY: each read names its table, and the collection read returns that table's rows.
  const history = await read("environment_deployment") as (typeof schema.environmentDeployment.$inferSelect)[];
  const snapshots = await read("environment_node_config_snapshot") as (typeof schema.environmentNodeConfigSnapshot.$inferSelect)[];
  const attempt = history.find((candidate) => candidate.id === target);
  if (!attempt) throw new Error("Missing attempt");
  const result = attemptTarget({ attempt, progress: attempt.runtimeProgress, history, snapshots });
  expect(result.nodes.map(({ nodeId, changed, removed: gone }) => ({ nodeId, changed, removed: gone ?? false }))).toEqual([
    { nodeId: api, changed: true, removed: false },
    { nodeId: web, changed: true, removed: false },
    { nodeId: data, changed: false, removed: false },
    { nodeId: removed, changed: true, removed: true },
  ]);
  const view = deploymentView({ deployment: { ...attempt, deployPreview: {} }, progress: result.progress, nodes: result.nodes });
  expect(Object.fromEntries(view.nodes.map((node) => [node.nodeId, node.outcome]))).toEqual({
    [api]: "deployed", [web]: "failed", [data]: "unchanged", [removed]: "removed",
  });
  expect(view.nodes.find((node) => node.nodeId === web)?.failure).toEqual({ message: "Health check timed out", containerId: "c0ffee" });
});
