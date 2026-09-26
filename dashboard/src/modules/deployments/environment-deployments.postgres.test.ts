import { afterAll, beforeAll, expect, it } from "vitest";
import * as schema from "#/db/schema";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";
import { getDeploymentAttempt, listEnvironmentDeployments } from "./deployment-operations.server";

const organizationId = "00000000-0000-4000-8000-000000000b01";
const userId = "00000000-0000-4000-8000-000000000b02";
const projectId = "00000000-0000-4000-8000-000000000b03";
const environmentId = "00000000-0000-4000-8000-000000000b04";
const api = "00000000-0000-4000-8000-000000000b21";
const data = "00000000-0000-4000-8000-000000000b22";
const id = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
// Inserted in shuffled id order, so ties on creation time must break by id rather than insertion.
const attempts = Array.from({ length: 45 }, (_, index) => ({ id: id(1000 + ((index * 7) % 45)), minute: Math.floor(index / 3) }));

let harness: PostgresTestHarness;
beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
afterAll(async () => { await harness?.stop(); });

it("pages an environment's attempts 20 at a time, newest first, ties broken by id", async () => {
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
  await harness.db.insert(schema.environmentDeployment).values(attempts.map((attempt) => ({
    id: attempt.id, organizationId, environmentId, savedStateSnapshotId: saved?.id ?? "", status: "applied" as const,
    triggerOrigin: { origin: "manual" as const, actorId: userId }, createdAt: new Date(Date.UTC(2026, 8, 1, 0, attempt.minute)),
  })));
  const read = (before?: string) => harness.runEffect(listEnvironmentDeployments({ userId }, { organizationSlug: "acme", environmentId, before }));
  const newestFirst = [...attempts].sort((a, b) => b.minute - a.minute || b.id.localeCompare(a.id)).map((attempt) => attempt.id);

  const page1 = await read();
  expect(page1.items.map((row) => row.id)).toEqual(newestFirst.slice(0, 20));
  expect(page1.items[0]).toMatchObject({ projectSlug: "shop", environmentSlug: "production", canRetry: false });
  expect(page1.next).toBe(newestFirst[19]);
  const page2 = await read(page1.next ?? undefined);
  expect(page2.items.map((row) => row.id)).toEqual(newestFirst.slice(20, 40));
  const page3 = await read(page2.next ?? undefined);
  expect(page3.items.map((row) => row.id)).toEqual(newestFirst.slice(40));
  expect(page3.next).toBeNull();
});

it("reads one attempt with the service configs it deployed, without sealed ciphertext", async () => {
  const attempt = attempts[0]?.id ?? "";
  await harness.db.insert(schema.environmentNodeConfigSnapshot).values([{
    organizationId, environmentId, environmentDeploymentId: attempt, nodeType: "service", nodeId: api, nodeLineageId: api,
    config: { privateDns: "api", source: { type: "image", image: "nginx:1" }, env: [{ key: "TOKEN", value: { type: "sealed", encryptedValue: "ciphertext", fingerprint: "f" } }] },
  }, {
    organizationId, environmentId, environmentDeploymentId: attempt, nodeType: "volume", nodeId: data, nodeLineageId: data, config: { version: 2, name: "data" },
  }]);
  const read = (deploymentId: string) => harness.runEffect(getDeploymentAttempt({ userId }, { organizationSlug: "acme", deploymentId }));

  const found = await read(attempt);
  expect(found?.row).toMatchObject({ id: attempt, environmentId, projectSlug: "shop" });
  expect(found?.serviceConfigs.map((config) => config.nodeId)).toEqual([api]);
  expect(JSON.stringify(found?.serviceConfigs)).not.toContain("ciphertext");
  expect(await read(id(9999))).toBeNull();
});
