import { randomUUID } from "node:crypto";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { parseServiceConfig, type ServiceManagedHostname, type ServiceSource } from "@ployz/sdk/config";
import * as schema from "#/db/schema";
import { InngestClient } from "#/modules/inngest/client";
import { makeSecretEncryption, SecretEncryption } from "#/utils/encrypted-secret.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";
import { createBranch } from "./branch-operations.server";
import type { CreateBranch } from "./branch-schemas";

const organizationId = "00000000-0000-4000-8000-000000000301";
const userId = "00000000-0000-4000-8000-000000000302";
const projectId = "00000000-0000-4000-8000-000000000303";
const parentId = "00000000-0000-4000-8000-000000000304";
const webLineage = "00000000-0000-4000-8000-000000000311";
const webId = "00000000-0000-4000-8000-000000000312";
const dbLineage = "00000000-0000-4000-8000-000000000321";
const dbId = "00000000-0000-4000-8000-000000000322";
const dataLineage = "00000000-0000-4000-8000-000000000331";
const dataId = "00000000-0000-4000-8000-000000000332";
const tokenId = "00000000-0000-4000-8000-000000000341";
const urlId = "00000000-0000-4000-8000-000000000342";
const encrypted = { version: 1 as const, iv: "iv", tag: "tag", ciphertext: "parent-cipher" };
const policy = { autoDeploy: false };

function config(slug: string, source: ServiceSource, managedHostnames: ServiceManagedHostname[] = []) {
  const { env: _env, mounts: _mounts, ...parsed } = parseServiceConfig({
    version: 2, source, healthcheck: { type: "none" }, restartPolicy: "unless-stopped", privateDns: slug, managedHostnames,
  });
  return parsed;
}

/** production: web (credential, managed hostname, a secret, a reference to db) uses db, which mounts data. Never deployed. */
const parentIntent = {
  version: 1, environmentSlug: "shop-production",
  services: [{
    id: webId, lineageId: webLineage, slug: "web",
    config: config("web", { version: 1, type: "image", image: "web:1", credentials: { type: "configured", credentialId: webId } }, [{ prefix: "web", targetPort: null }]),
    variables: [
      { id: tokenId, key: "TOKEN", description: null, exported: false, valueFingerprint: "fp-token", value: { kind: "secret", encryptedValue: null } },
      { id: urlId, key: "DB_URL", description: null, exported: false, valueFingerprint: "fp-url",
        value: { kind: "template", parts: [{ kind: "ref", owner: { scope: "service", lineageId: dbLineage }, key: "PLOYZ_PRIVATE_DOMAIN" }] } },
    ],
    volumeAttachments: [],
  }, {
    id: dbId, lineageId: dbLineage, slug: "db",
    config: config("db", { version: 1, type: "image", image: "postgres:17", credentials: { type: "none" } }),
    variables: [], volumeAttachments: [{ volumeResourceId: dataId, mountPath: "/var/lib/postgresql" }],
  }],
  volumes: [{ resourceId: dataId, resourceLineageId: dataLineage, name: "data" }],
};

describe("createBranch", () => {
  let harness: PostgresTestHarness;
  const inngest = new Inngest({ id: "create-branch-test" });
  vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

  beforeAll(async () => {
    harness = await startPostgresTestHarness();
  }, 60_000);
  afterAll(async () => {
    await harness?.stop();
  });

  beforeEach(async () => {
    vi.mocked(inngest.send).mockReset().mockResolvedValue({ ids: [] });
    await harness.pool.query(`
      truncate table organization, "user" cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
      insert into "user" (id, email, name) values ('${userId}', 'owner@example.com', 'Owner');
      insert into project (id, organization_id, name, slug) values ('${projectId}', '${organizationId}', 'Shop', 'shop');
      insert into environment (id, project_id, organization_id, name, namespace, intent)
        values ('${parentId}', '${projectId}', '${organizationId}', 'production', 'shop-production', '${JSON.stringify(parentIntent)}');
      insert into service_lineage (id, organization_id, project_id, canonical_name, canonical_slug) values
        ('${webLineage}', '${organizationId}', '${projectId}', 'Web', 'web'),
        ('${dbLineage}', '${organizationId}', '${projectId}', 'DB', 'db');
      insert into service (id, project_id, environment_id, organization_id, lineage_id, name, policy, has_registry_credential) values
        ('${webId}', '${projectId}', '${parentId}', '${organizationId}', '${webLineage}', 'Storefront', '${JSON.stringify(policy)}', true),
        ('${dbId}', '${projectId}', '${parentId}', '${organizationId}', '${dbLineage}', 'Postgres', '${JSON.stringify(policy)}', false);
      insert into service_registry_credential (organization_id, service_id, encrypted_registry_secret)
        values ('${organizationId}', '${webId}', '${JSON.stringify(encrypted)}');
      insert into resource_lineage (id, organization_id, project_id, canonical_name, canonical_slug)
        values ('${dataLineage}', '${organizationId}', '${projectId}', 'data', 'data');
      insert into environment_resource (id, organization_id, project_id, environment_id, lineage_id, implementation_type)
        values ('${dataId}', '${organizationId}', '${projectId}', '${parentId}', '${dataLineage}', 'volume');
      insert into environment_canvas_node_position (organization_id, environment_id, resource_type, resource_id, x, y) values
        ('${organizationId}', '${parentId}', 'service', '${webId}', 10, 20),
        ('${organizationId}', '${parentId}', 'volume', '${dataId}', 30, 40);
      insert into variable (id, organization_id, environment_id, service_id) values
        ('${tokenId}', '${organizationId}', '${parentId}', '${webId}'),
        ('${urlId}', '${organizationId}', '${parentId}', '${webId}');
      insert into variable_secret (organization_id, environment_id, variable_id, encrypted_value)
        values ('${organizationId}', '${parentId}', '${tokenId}', '${JSON.stringify(encrypted)}');
    `);
    await harness.db.insert(schema.member).values({ id: randomUUID(), userId, organizationId, role: "owner" });
  });

  function create(input: Partial<CreateBranch> = {}) {
    return harness.runEffect(createBranch({ userId }, {
      organizationSlug: "acme", parentEnvironmentId: parentId, name: "fix-web",
      focus: [webLineage], picks: { preset: "only" }, keep: false, setupCommands: [], ...input,
    }).pipe(
      Effect.provideService(SecretEncryption, makeSecretEncryption("test-encryption-secret")),
      Effect.provideService(InngestClient, inngest),
    ));
  }

  it("copies the picked nodes under fresh ids, stores the base and admits the first deployment", async () => {
    const { data } = await create({ keep: true, setupCommands: [{ lineageId: webLineage, command: "pnpm db:seed" }] });
    const branchId = data.environment.id;
    expect(data.environment.namespace).toBe("shop-fix-web");

    // Own Copies: the Parent's lineage under fresh ids. The Parent was never deployed, so db and its Volume come along.
    const services = await harness.db.select().from(schema.service).where(eq(schema.service.environmentId, branchId));
    expect(services).toHaveLength(2);
    expect(services).toEqual(expect.arrayContaining([
      expect.objectContaining({ lineageId: dbLineage, name: "Postgres", policy, hasRegistryCredential: false }),
      expect.objectContaining({ lineageId: webLineage, name: "Storefront", policy, hasRegistryCredential: true }),
    ]));
    const serviceIds = services.map((row) => row.id);
    expect(serviceIds).not.toContain(webId);
    expect(serviceIds).not.toContain(dbId);
    const volumes = await harness.db.select().from(schema.environmentResource).where(eq(schema.environmentResource.environmentId, branchId));
    expect(volumes).toEqual([expect.objectContaining({ lineageId: dataLineage })]);
    const volumeIds = volumes.map((row) => row.id);
    expect(volumeIds).not.toContain(dataId);

    // Credentials, positions and sealed values are copied; the managed hostname follows the name.
    const webNode = data.environment.intent.services.find((node) => node.lineageId === webLineage);
    expect(webNode?.config.managedHostnames[0]?.prefix).toBe("web-fix-web");
    const credentials = await harness.db.select().from(schema.serviceRegistryCredential).where(eq(schema.serviceRegistryCredential.serviceId, webNode?.id ?? ""));
    expect(credentials.map((row) => row.encryptedRegistrySecret)).toEqual([encrypted]);
    const positions = await harness.db.select().from(schema.environmentCanvasNodePosition).where(eq(schema.environmentCanvasNodePosition.environmentId, branchId));
    expect(positions).toHaveLength(2);
    expect(positions).toEqual(expect.arrayContaining([
      expect.objectContaining({ resourceId: webNode?.id, x: 10, y: 20 }),
      expect.objectContaining({ resourceId: volumeIds[0], x: 30, y: 40 }),
    ]));
    const token = webNode?.variables.find((variable) => variable.key === "TOKEN");
    expect(token?.id).not.toBe(tokenId);
    const secrets = await harness.db.select().from(schema.variableSecret).where(eq(schema.variableSecret.variableId, token?.id ?? ""));
    expect(secrets.map((row) => row.encryptedValue)).toEqual([encrypted]);

    // The base and the Node Introductions.
    const branches = await harness.db.select().from(schema.environmentBranch).where(eq(schema.environmentBranch.environmentId, branchId));
    expect(branches).toEqual([expect.objectContaining({
      parentEnvironmentId: parentId, kept: true, createdByUserId: userId, setupCommands: [{ lineageId: webLineage, command: "pnpm db:seed" }],
    })]);
    const base = branches[0]?.base;
    expect(base?.services.map((node) => node.id)).toEqual(expect.arrayContaining([dbId, webId]));
    expect(JSON.stringify(base)).not.toContain("parent-cipher");
    const introductions = await harness.db.select().from(schema.environmentNodeIntroduction)
      .where(eq(schema.environmentNodeIntroduction.environmentId, branchId));
    expect(introductions).toHaveLength(3);
    expect(introductions.map((row) => row.nodeId)).toEqual(expect.arrayContaining([...serviceIds, ...volumeIds]));

    // Its first deployment is admitted and dispatched.
    const attempts = await harness.db.select().from(schema.environmentDeployment).where(eq(schema.environmentDeployment.environmentId, branchId));
    expect(attempts.map((attempt) => attempt.id)).toEqual([data.deploymentId]);
    // Admission froze web's Setup Command on it: web has never deployed.
    expect(attempts[0]?.setupCommands).toEqual({ [webNode?.id ?? ""]: ["pnpm db:seed"] });
    expect(inngest.send).toHaveBeenCalledTimes(1);
  });

  it("refuses a taken or too-long name, and nothing to copy, before writing anything", async () => {
    await create();
    await expect(create()).rejects.toMatchObject({ _tag: "Conflict" });
    await expect(create({ name: "x".repeat(60) })).rejects.toMatchObject({ _tag: "Validation", field: "name" });
    await expect(create({ name: "empty", focus: [] })).rejects.toMatchObject({ _tag: "Validation", field: "picks" });
    await expect(create({ name: "stray", setupCommands: [{ lineageId: dataLineage, command: "seed" }] }))
      .rejects.toMatchObject({ _tag: "Validation", field: "setupCommands" });
    expect(await harness.db.select().from(schema.environment)).toHaveLength(2);
  });
});
