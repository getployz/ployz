import { afterAll, beforeAll, expect, it } from "vitest";
import * as schema from "#/db/schema";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";
import type { DeploymentProgress } from "./deployment-progress";
import { listNodeDeployments } from "./deployment-operations.server";

const organizationId = "00000000-0000-4000-8000-000000000a01";
const userId = "00000000-0000-4000-8000-000000000a02";
const projectId = "00000000-0000-4000-8000-000000000a03";
const environmentId = "00000000-0000-4000-8000-000000000a04";
const api = "00000000-0000-4000-8000-000000000a21";
const worker = "00000000-0000-4000-8000-000000000a22";
const id = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const [first, second, third, listless] = [id(1), id(2), id(3), id(4)];
const extra = Array.from({ length: 30 }, (_, index) => id(100 + index));

/** A service in an attempt's frozen target list. */
const target = (nodeId: string, name: string, changed: boolean) => ({ nodeId, nodeType: "service" as const, name, changed, removed: false, needsBuild: false, source: null, mounts: [] });
const failedApi: DeploymentProgress = { completed: 0, total: 1, outcome: "failed", compensation: [], rows: [{ index: 0, machineId: "m", machineName: "server",
  serviceId: api, runtimeServiceId: null, serviceName: "api", displayName: null, operation: "replace_container", target: "c1", updateOrder: null,
  status: "failed", phase: null, elapsedMs: null, deadlineMs: null, health: null, error: "health check failed", containerId: "c1", startedAt: null, finishedAt: null }] };

let harness: PostgresTestHarness;
beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
afterAll(async () => { await harness?.stop(); });

it("reads a node's Running attempt and its changing History, 20 a page", async () => {
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
  const deployment = (deploymentId: string, minute: number, status: "applied" | "failed", nodes: ReturnType<typeof target>[], runtimeProgress: DeploymentProgress | null = null) => ({
    id: deploymentId, organizationId, environmentId, savedStateSnapshotId: saved?.id ?? "", status, message: deploymentId,
    triggerOrigin: { origin: "manual" as const, actorId: userId }, createdAt: new Date(Date.UTC(2026, 8, 1, 0, minute)),
    targetNodes: { version: 1 as const, nodes }, runtimeProgress,
  });
  // `first` deployed api; `second` only added worker (api Unchanged); `third` failed api's health check, so `first` still serves it.
  // Then 30 more failed attempts in pairs that share a creation time, every fifth leaving api unchanged.
  await harness.db.insert(schema.environmentDeployment).values([
    deployment(first, 1, "applied", [target(api, "api", true)]),
    deployment(second, 2, "applied", [target(api, "api", false), target(worker, "worker", true)]),
    deployment(third, 3, "failed", [target(api, "api", true), target(worker, "worker", false)], failedApi),
    // Migrated from before Target Node Lists: an empty list, so it never shows.
    deployment(listless, 4, "applied", []),
    ...extra.map((deploymentId, index) => deployment(deploymentId, 10 + Math.floor(index / 2), "failed", [target(api, "api", index % 5 !== 0)])),
  ]);
  const read = (before?: string) => harness.runEffect(listNodeDeployments({ userId }, { organizationSlug: "acme", environmentId, nodeId: api, before }));

  const shown = extra.filter((_, index) => index % 5 !== 0).reverse();
  const page1 = await read();
  expect(page1.running).toMatchObject({ id: first, outcome: "deployed", message: first });
  expect(page1.items.map((item) => item.id)).toEqual(shown.slice(0, 20));
  expect(page1.next).toBe(shown[19]);

  // Later pages skip the Running scan; History holds the Running attempt too, which the panel shows once.
  const page2 = await read(page1.next ?? undefined);
  expect(page2.running).toBeNull();
  expect(page2.items.map((item) => item.id)).toEqual([...shown.slice(20), third, first]);
  expect(page2.items.map((item) => item.outcome).slice(-2)).toEqual(["failed", "deployed"]);
  expect(page2.next).toBeNull();
});
