import { afterAll, beforeAll, expect, it } from "vitest";
import { Effect } from "effect";
import * as schema from "#/db/schema";
import {
  createDefaultServiceHealthcheck, createDefaultServiceRestartPolicy, createImageServiceSource, projectServiceDeploymentConfig,
} from "#/modules/environment-design/services";
import type { EnvironmentSnapshotVariableProducer, ValuePart } from "#/modules/environment-design/tables";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";
import { makeSecretEncryption, SecretEncryption } from "#/utils/encrypted-secret.server";
import { getDeploymentServiceVariables } from "./deployment-operations.server";

const organizationId = "00000000-0000-4000-8000-000000000a01";
const userId = "00000000-0000-4000-8000-000000000a02";
const projectId = "00000000-0000-4000-8000-000000000a03";
const environmentId = "00000000-0000-4000-8000-000000000a04";
const [first, later] = ["a11", "a12"].map((suffix) => `00000000-0000-4000-8000-000000000${suffix}`) as [string, string];
const [api, web] = ["a21", "a22"].map((suffix) => `00000000-0000-4000-8000-000000000${suffix}`) as [string, string];
const [apiLineage, webLineage] = ["a31", "a32"].map((suffix) => `00000000-0000-4000-8000-000000000${suffix}`) as [string, string];
const encryption = makeSecretEncryption("test-encryption-secret");
const sealed = encryption.encrypt("hunter2");

const text = (value: string): ValuePart => ({ kind: "text", value });
const selfRef = (key: string): ValuePart => ({ kind: "ref", owner: { scope: "self" }, key });
const apiRef = (key: string): ValuePart => ({ kind: "ref", owner: { scope: "service", lineageId: apiLineage }, key });

/** One attempt's frozen inputs: api's APP_URL literal, a sealed SECRET, a template over it, and web reading api. */
async function admit(deploymentId: string, appUrl: string, minute: number, savedStateSnapshotId: string) {
  await harness.db.insert(schema.environmentDeployment).values({
    id: deploymentId, organizationId, environmentId, savedStateSnapshotId, status: "applied",
    triggerOrigin: { origin: "manual", actorId: userId }, createdAt: new Date(Date.UTC(2026, 8, 1, 0, minute)),
    variableProducers: [
      { ownerScope: "service", ownerId: api, ownerLineageId: apiLineage, key: "APP_URL", value: { kind: "literal", value: appUrl } },
      { ownerScope: "service", ownerId: api, ownerLineageId: apiLineage, key: "SECRET", value: { kind: "secret", encryptedValue: sealed } },
      { ownerScope: "service", ownerId: api, ownerLineageId: apiLineage, key: "DATABASE_URL", value: { kind: "template", parts: [text("postgres://app:"), selfRef("SECRET"), text("@db")] } },
    ] satisfies EnvironmentSnapshotVariableProducer[],
  });
  const config = (privateDns: string, env: ReturnType<typeof projectServiceDeploymentConfig>["env"]) => ({
    ...projectServiceDeploymentConfig({
      source: createImageServiceSource({ image: "nginx:1" }), preDeployCommand: null, startCommand: null,
      healthcheck: createDefaultServiceHealthcheck(), restartPolicy: createDefaultServiceRestartPolicy(), privateDns,
    }),
    env,
    // A custom domain: deploy adds PLOYZ_PUBLIC_DOMAIN, which no one authored.
    routes: privateDns === "api" ? [{ id: "00000000-0000-4000-8000-000000000a41", hostname: "api.example.test", targetPort: null }] : [],
  });
  await harness.db.insert(schema.environmentNodeConfigSnapshot).values([
    { organizationId, environmentId, environmentDeploymentId: deploymentId, nodeType: "service", nodeId: api, nodeLineageId: apiLineage, config: config("api", {
      APP_URL: { kind: "literal", value: appUrl },
      SECRET: { kind: "secret", fingerprint: encryption.sealedFingerprint("hunter2"), encryptedValue: sealed },
      DATABASE_URL: { kind: "literal", value: "postgres://app:${{ SECRET }}@db", parts: [text("postgres://app:"), selfRef("SECRET"), text("@db")] },
    }) },
    { organizationId, environmentId, environmentDeploymentId: deploymentId, nodeType: "service", nodeId: web, nodeLineageId: webLineage, config: config("web", {
      API_URL: { kind: "literal", value: "${{ api.APP_URL }}/v1", parts: [apiRef("APP_URL"), text("/v1")] },
      DB: { kind: "literal", value: "${{ api.DATABASE_URL }}", parts: [apiRef("DATABASE_URL")] },
    }) },
  ]);
}

let harness: PostgresTestHarness;
beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
afterAll(async () => { await harness?.stop(); });

it("recomputes an attempt's values from its frozen inputs, sealing anything that resolves from a sealed value", async () => {
  await harness.pool.query(`
    insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
    insert into "user" (id, email, name) values ('${userId}', 'owner@example.com', 'Owner');
    insert into member (user_id, organization_id, role) values ('${userId}', '${organizationId}', 'owner');
    insert into project (id, organization_id, name, slug) values ('${projectId}', '${organizationId}', 'Shop', 'shop');
    insert into environment (id, project_id, organization_id, name, namespace, intent) values ('${environmentId}', '${projectId}', '${organizationId}',
      'Production', 'production', '{"version":1,"environmentSlug":"production","services":[],"volumes":[]}');
    insert into service_lineage (id, organization_id, project_id, canonical_name, canonical_slug) values
      ('${apiLineage}', '${organizationId}', '${projectId}', 'api', 'api'), ('${webLineage}', '${organizationId}', '${projectId}', 'web', 'web');
    insert into service (id, organization_id, project_id, environment_id, lineage_id, name) values
      ('${api}', '${organizationId}', '${projectId}', '${environmentId}', '${apiLineage}', 'api'),
      ('${web}', '${organizationId}', '${projectId}', '${environmentId}', '${webLineage}', 'web');
  `);
  const [saved] = await harness.db.insert(schema.environmentSavedStateSnapshot).values({
    organizationId, environmentId, actorId: userId, intent: { version: 1, environmentSlug: "production", services: [], volumes: [] }, volumeDeletionAuthorizations: [],
  }).returning();
  await admit(first, "https://old.example.test", 1, saved?.id ?? "");
  // The variables change afterwards, and a later attempt deploys the new value.
  await admit(later, "https://new.example.test", 2, saved?.id ?? "");

  const read = (deploymentId: string, serviceId: string) =>
    harness.runEffect(getDeploymentServiceVariables({ userId }, { organizationSlug: "acme", deploymentId, serviceId }).pipe(
      // Loading the saved intent asks for the service; the read itself must never decrypt.
      Effect.provideService(SecretEncryption, { ...encryption, decrypt: () => { throw new Error("decrypted a sealed value"); } }),
    ));
  const apiValues = await read(first, api);
  const webValues = await read(first, web);
  expect(apiValues).toEqual({ PLOYZ_PUBLIC_DOMAIN: "api.example.test", APP_URL: "https://old.example.test", SECRET: null, DATABASE_URL: null });
  expect(webValues).toEqual({ API_URL: "https://old.example.test/v1", DB: null });
  expect(await read(later, web)).toMatchObject({ API_URL: "https://new.example.test/v1" });

  const response = JSON.stringify([apiValues, webValues]);
  for (const leak of ["hunter2", sealed.ciphertext, sealed.iv, sealed.tag]) expect(response).not.toContain(leak);
});
