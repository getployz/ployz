import { afterAll, beforeAll, expect, it } from "vitest";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import type { MachineId } from "@ployz/sdk";
import * as schema from "#/db/schema";
import {
  createDefaultServiceHealthcheck, createDefaultServiceRestartPolicy, createImageServiceSource, projectServiceDeploymentConfig,
} from "#/modules/environment-design/services";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";
import { makeSecretEncryption, SecretEncryption } from "#/utils/encrypted-secret.server";
import { writeTargetNodeList } from "./attempt-target.server";
import { loadEnvironmentSnapshotProjection } from "./environment-state.repository.server";
import { viewTargetNodes, deploymentView } from "./deployment-view";

const organizationId = "00000000-0000-4000-8000-000000000901";
const userId = "00000000-0000-4000-8000-000000000902";
const projectId = "00000000-0000-4000-8000-000000000903";
const environmentId = "00000000-0000-4000-8000-000000000904";
const [applied, failed, target] = ["911", "912", "913"].map((suffix) => `00000000-0000-4000-8000-000000000${suffix}`) as [string, string, string];
const [api, web, old, data, build] = ["921", "922", "923", "924", "925"].map((suffix) => `00000000-0000-4000-8000-000000000${suffix}`) as [string, string, string, string, string];
const machineId = "a".repeat(32) as MachineId;
const encryption = makeSecretEncryption("test-encryption-secret");

const service = (privateDns: string, image = "nginx:1") => projectServiceDeploymentConfig({
  source: createImageServiceSource({ image }), preDeployCommand: null, startCommand: null,
  healthcheck: createDefaultServiceHealthcheck(), restartPolicy: createDefaultServiceRestartPolicy(), privateDns,
});
const removeContainer = (serviceName: string, index: number) => ({
  index, machine_id: machineId, machine_name: null, display_name: null, service_name: serviceName, status: { type: "pending" },
  operation: { type: "remove_container", machine_id: machineId, container_id: String(index).repeat(64) },
});

let harness: PostgresTestHarness;
beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
afterAll(async () => { await harness?.stop(); });

it("writes the target node list against Applied State, counting a failed attempt's confirmed services", async () => {
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
  const at = (minute: number) => new Date(Date.UTC(2026, 8, 1, 0, minute));
  const deployment = (id: string, status: "applied" | "failed" | "queued", minute: number) => ({
    id, organizationId, environmentId, savedStateSnapshotId: saved?.id ?? "", status,
    triggerOrigin: { origin: "manual" as const, actorId: userId }, createdAt: at(minute),
    finishedAt: status === "queued" ? null : at(minute),
  });
  // The failed attempt planned new api and web; only api's operations completed.
  const preview = {
    project_name: "production", operations: [removeContainer("api", 0), removeContainer("web", 1)], warnings: [], would_remove: [], preserved_volumes: [],
  };
  const [apiOperation, webOperation] = preview.operations.map((row) => row.operation);
  const outcome = { version: 1, outcome: { type: "failed", completed: [apiOperation], unexecuted: [], failed: {
    type: "operation", operation: webOperation, error: { type: "cancelled" },
  } } };
  await harness.db.insert(schema.environmentDeployment).values([
    deployment(applied, "applied", 1), { ...deployment(failed, "failed", 2), deployPreview: preview }, deployment(target, "queued", 3),
  ]);
  await harness.db.insert(schema.environmentDeploymentSecret).values({
    organizationId, environmentDeploymentId: failed,
    encryptedRuntimeOutcome: encryption.encrypt(JSON.stringify(outcome)),
  });
  const snapshot = (deploymentId: string, nodeId: string, config: typeof schema.environmentNodeConfigSnapshot.$inferInsert["config"], nodeType: "service" | "volume" = "service") => ({
    organizationId, environmentId, environmentDeploymentId: deploymentId, nodeType, nodeId, nodeLineageId: nodeId, config,
  });
  await harness.db.insert(schema.environmentNodeConfigSnapshot).values([
    snapshot(applied, api, service("api")), snapshot(applied, web, service("web")), snapshot(applied, old, service("old")),
    snapshot(applied, data, { version: 2, name: "data" }, "volume"),
    snapshot(failed, api, service("api", "nginx:2")), snapshot(failed, web, service("web", "nginx:2")),
    snapshot(target, api, service("api", "nginx:2")), snapshot(target, web, { ...service("web", "nginx:2"), mounts: [{ volumeResourceId: data, target: "/data" }] }),
    snapshot(target, data, { version: 2, name: "data" }, "volume"),
    snapshot(target, build, { ...service("build"), source: { type: "git", repository: "acme/build" } }),
  ]);

  // As the attempt's start writes it: against the whole of Applied State.
  await harness.runTransaction(() => loadEnvironmentSnapshotProjection({ kind: "environment", environmentId }).pipe(
    Effect.flatMap((projection) => writeTargetNodeList(target, projection.appliedSavedNodeByKey)),
    Effect.provideService(SecretEncryption, encryption),
  ));

  const [row] = await harness.db.select().from(schema.environmentDeployment).where(eq(schema.environmentDeployment.id, target));
  expect(row?.targetNodes.version).toBe(1);
  expect(Object.fromEntries(row?.targetNodes.nodes.map((node) => [node.nodeId, node]) ?? [])).toEqual({
    // The failed attempt confirmed api at nginx:2, so it is Applied State: unchanged.
    [api]: { nodeId: api, nodeType: "service", name: "api", changed: false, removed: false, needsBuild: false, source: { kind: "image", label: "nginx:2" }, mounts: [] },
    // Its web never finished, so Applied State still has nginx:1.
    [web]: { nodeId: web, nodeType: "service", name: "web", changed: true, removed: false, needsBuild: false, source: { kind: "image", label: "nginx:2" }, mounts: [data] },
    [data]: { nodeId: data, nodeType: "volume", name: "data", changed: false, removed: false, needsBuild: false, source: null, mounts: [] },
    [build]: { nodeId: build, nodeType: "service", name: "build", changed: true, removed: false, needsBuild: true, source: { kind: "git", label: "acme/build" }, mounts: [] },
    [old]: { nodeId: old, nodeType: "service", name: "old", changed: true, removed: true, needsBuild: false, source: { kind: "image", label: "nginx:1" }, mounts: [] },
  });

  if (!row) throw new Error("Missing target attempt");
  const { nodes, progress } = viewTargetNodes(row.targetNodes, null);
  const view = deploymentView({ deployment: { status: "applied", failureMessage: null, planned: true }, progress, nodes });
  expect(Object.fromEntries(view.nodes.map((node) => [node.nodeId, node.outcome]))).toEqual({
    [api]: "unchanged", [web]: "deployed", [data]: "unchanged", [build]: "deployed", [old]: "removed",
  });
});
