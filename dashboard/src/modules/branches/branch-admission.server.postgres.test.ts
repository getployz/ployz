import { afterAll, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { eq } from "drizzle-orm";
import { parseServiceConfig } from "@ployz/sdk/config";
import * as schema from "#/db/schema";
import { compileSavedEnvironmentIntent, type SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { admitEnvironmentDeployment } from "#/modules/deployments/admission.server";
import { loadDeploymentContext } from "#/modules/deployments/runtime-repository.server";
import { compileRuntimeIntent } from "#/modules/deployments/runtime-session.server";
import type { compileSdkPreparationInput } from "#/modules/deployments/runtime-preview";
import { makeSecretEncryption, SecretEncryption } from "#/utils/encrypted-secret.server";
import { Effect } from "effect";
import type { Database } from "#/server/database.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const organizationId = "00000000-0000-4000-8000-000000000a01";
const userId = "00000000-0000-4000-8000-000000000a02";
const projectId = "00000000-0000-4000-8000-000000000a03";
const productionId = "00000000-0000-4000-8000-000000000a04";
const branchId = "00000000-0000-4000-8000-000000000a05";
const webLineage = "00000000-0000-4000-8000-000000000a11";
const dbLineage = "00000000-0000-4000-8000-000000000a12";
const cacheLineage = "00000000-0000-4000-8000-000000000a13";
const prodWeb = "00000000-0000-4000-8000-000000000a21";
const prodDb = "00000000-0000-4000-8000-000000000a22";
const branchWeb = "00000000-0000-4000-8000-000000000a31";
const encryption = makeSecretEncryption("test-encryption-secret");

const config = (slug: string) => {
  const { env: _env, mounts: _mounts, ...parsed } = parseServiceConfig({
    version: 2, source: { version: 1, type: "image", image: `${slug}:1`, credentials: { type: "none" } },
    healthcheck: { type: "none" }, restartPolicy: "unless-stopped", privateDns: slug, managedHostnames: [],
  });
  return parsed;
};
const ref = (lineageId: string, key: string) => ({ kind: "template" as const, parts: [{ kind: "ref" as const, owner: { scope: "service" as const, lineageId }, key }] });
const variable = (id: string, key: string, value: SavedEnvironmentIntent["services"][number]["variables"][number]["value"]) =>
  ({ id, key, description: null, exported: false, valueFingerprint: `fp-${key}`, value });
const id = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;

/** production runs web and db; db reads web's address and holds a sealed password. */
const production = (): SavedEnvironmentIntent => ({
  version: 1, environmentSlug: "shop-production", volumes: [],
  services: [
    { id: prodWeb, lineageId: webLineage, slug: "web", config: config("web"), variables: [], volumeAttachments: [] },
    { id: prodDb, lineageId: dbLineage, slug: "db", config: config("db"), volumeAttachments: [], variables: [
      variable(id(1), "WEB_ORIGIN", ref(webLineage, "PLOYZ_PRIVATE_DOMAIN")),
      variable(id(2), "PASSWORD", { kind: "secret", encryptedValue: encryption.encrypt("hunter2") }),
    ] },
  ],
});
/** The Branch owns web; db is live from production; nothing runs cache. */
const branch = (): SavedEnvironmentIntent => ({
  version: 1, environmentSlug: "shop-fix-web", volumes: [],
  services: [{ id: branchWeb, lineageId: webLineage, slug: "web", config: config("web"), volumeAttachments: [], variables: [
    variable(id(3), "DB_HOST", ref(dbLineage, "PLOYZ_PRIVATE_DOMAIN")),
    variable(id(4), "DB_SEES_WEB", ref(dbLineage, "WEB_ORIGIN")),
    variable(id(5), "DB_PASSWORD", ref(dbLineage, "PASSWORD")),
    variable(id(6), "DB_MISSING", ref(dbLineage, "NOPE")),
    variable(id(7), "CACHE_HOST", ref(cacheLineage, "PLOYZ_PRIVATE_DOMAIN")),
  ] }],
});

describe("branchAdmission", () => {
  let harness: PostgresTestHarness;
  beforeAll(async () => {
    harness = await startPostgresTestHarness();
  }, 60_000);
  afterAll(async () => {
    await harness?.stop();
  });

  async function saved(environmentId: string, intent: SavedEnvironmentIntent) {
    const [row] = await harness.db.insert(schema.environmentSavedStateSnapshot)
      .values({ organizationId, environmentId, actorId: userId, intent, volumeDeletionAuthorizations: [] }).returning();
    return row?.id ?? "";
  }
  /** production's latest applied attempt: what the Branch's Live values come from. */
  async function applyProduction(intent: SavedEnvironmentIntent) {
    const compiled = compileSavedEnvironmentIntent({ environmentId: productionId, intent });
    const [attempt] = await harness.db.insert(schema.environmentDeployment).values({
      organizationId, environmentId: productionId, savedStateSnapshotId: await saved(productionId, intent), status: "applied",
      triggerOrigin: { origin: "manual", actorId: userId }, variableProducers: compiled.variableProducers,
    }).returning();
    await harness.db.insert(schema.environmentNodeConfigSnapshot).values(compiled.nodeSnapshots.map((node) => ({
      organizationId, environmentDeploymentId: attempt?.id ?? "", environmentId: productionId, nodeType: node.nodeType,
      nodeId: node.nodeId, nodeLineageId: node.nodeLineageId, configVersion: node.configVersion, config: node.config,
    })));
  }
  type TriggerOrigin = Parameters<typeof admitEnvironmentDeployment>[0]["triggerOrigin"];
  async function admitBranch(triggerOrigin: TriggerOrigin = { origin: "manual", actorId: userId }) {
    const savedStateSnapshotId = await saved(branchId, branch());
    const admitted = await harness.runTransaction(() => admitEnvironmentDeployment({
      environmentId: branchId, savedStateSnapshotId, triggerOrigin, message: null,
    }));
    const [row] = await harness.db.select().from(schema.environmentDeployment).where(eq(schema.environmentDeployment.id, admitted.id));
    // What the runtime receives. No managed hostnames, so the Cluster Domain is never reserved.
    const compiled = loadDeploymentContext(admitted.id).pipe(
      Effect.flatMap((context) => context ? compileRuntimeIntent(context) : Effect.die("No deployment context.")),
      Effect.provideService(SecretEncryption, encryption),
    );
    const input = await harness.runEffect(compiled as Effect.Effect<ReturnType<typeof compileSdkPreparationInput>, unknown, Database>);
    return { row, env: input.snapshots.find((snapshot) => snapshot.serviceId === branchWeb)?.resolvedEnv, dependencies: input.dependencies };
  }

  beforeEach(async () => {
    const env = (envId: string, name: string, namespace: string) =>
      `('${envId}', '${projectId}', '${organizationId}', '${name}', '${namespace}', '{"version":1,"environmentSlug":"${namespace}","services":[],"volumes":[]}')`;
    await harness.pool.query(`
      truncate table organization, "user" cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
      insert into "user" (id, email, name) values ('${userId}', 'owner@example.com', 'Owner');
      insert into project (id, organization_id, name, slug) values ('${projectId}', '${organizationId}', 'Shop', 'shop');
      insert into environment (id, project_id, organization_id, name, namespace, intent) values
        ${env(productionId, "production", "shop-production")}, ${env(branchId, "fix-web", "shop-fix-web")};
      insert into environment_branch (environment_id, organization_id, project_id, parent_environment_id, base, created_by_user_id)
        values ('${branchId}', '${organizationId}', '${projectId}', '${productionId}', '{}', '${userId}');
      insert into service_lineage (id, organization_id, project_id, canonical_name, canonical_slug) values
        ('${webLineage}', '${organizationId}', '${projectId}', 'web', 'web-1'),
        ('${dbLineage}', '${organizationId}', '${projectId}', 'db', 'db-1'),
        ('${cacheLineage}', '${organizationId}', '${projectId}', 'cache', 'cache-1');
      insert into service (id, project_id, environment_id, organization_id, lineage_id, name) values
        ('${prodWeb}', '${projectId}', '${productionId}', '${organizationId}', '${webLineage}', 'web'),
        ('${prodDb}', '${projectId}', '${productionId}', '${organizationId}', '${dbLineage}', 'db'),
        ('${branchWeb}', '${projectId}', '${branchId}', '${organizationId}', '${webLineage}', 'web');
    `);
  });

  it("resolves Live references to the owner's values at its private addresses, adding no ordering", async () => {
    await applyProduction(production());
    const { row, env, dependencies } = await admitBranch();

    expect(env).toMatchObject({
      DB_HOST: "db.shop-production.internal",
      // db's own reference follows production's web, not the Branch's copy.
      DB_SEES_WEB: "web.shop-production.internal",
      DB_PASSWORD: "hunter2",
      DB_MISSING: "",
      CACHE_HOST: "",
    });
    // Sealed values stay sealed in the frozen producers until apply time.
    expect(JSON.stringify(row?.variableProducers)).not.toContain("hunter2");
    expect(dependencies).toEqual({});
    expect(row?.missingLiveValues).toEqual(expect.arrayContaining([
      { serviceId: branchWeb, from: "db", key: "NOPE" },
      { serviceId: branchWeb, from: "cache", key: "PLOYZ_PRIVATE_DOMAIN" },
    ]));
    expect(row?.missingLiveValues).toHaveLength(2);
  });

  it("deploys every Live reference empty when the Parent stopped running the service, and records them", async () => {
    await applyProduction({ ...production(), services: production().services.filter((service) => service.lineageId !== dbLineage) });
    const { row, env } = await admitBranch();

    expect(env).toMatchObject({ DB_HOST: "", DB_SEES_WEB: "", DB_PASSWORD: "", CACHE_HOST: "" });
    expect(row?.missingLiveValues.map((value) => `${value.from}.${value.key}`).sort()).toEqual([
      "cache.PLOYZ_PRIVATE_DOMAIN", "db.NOPE", "db.PASSWORD", "db.PLOYZ_PRIVATE_DOMAIN", "db.WEB_ORIGIN",
    ]);
  });

  it("captures fresh values on each admission, so the flag clears once the owner runs the service again", async () => {
    await applyProduction({ ...production(), services: production().services.filter((service) => service.lineageId !== dbLineage) });
    expect((await admitBranch()).row?.missingLiveValues).toHaveLength(5);
    await applyProduction(production());
    // A Git push goes through the same admission.
    const push: TriggerOrigin = { origin: "github", deliveryId: "d1", branchEvaluationRevision: 1, installationId: 17, repositoryId: 42 };
    const pushed = await admitBranch(push);
    expect(pushed.env).toMatchObject({ DB_HOST: "db.shop-production.internal" });
    expect(pushed.row?.missingLiveValues.map((value) => value.key).sort()).toEqual(["NOPE", "PLOYZ_PRIVATE_DOMAIN"]);
  });

  it("gives the owner's public addresses, directly or through its values, and keeps an address the owner itself uses live", async () => {
    const intent = production();
    const db = intent.services.find((service) => service.lineageId === dbLineage);
    if (db) {
      db.config.managedHostnames = [{ prefix: "db-shop", targetPort: null }];
      db.variables.push(variable(id(12), "CACHE_ADDR", ref(cacheLineage, "PLOYZ_PRIVATE_DOMAIN")));
      // db's WEB_URL embeds production's web's public address: the Branch reaches it only through db.
      db.variables.push(variable(id(13), "WEB_URL", { kind: "template", parts: [
        { kind: "text", value: "https://" }, { kind: "ref", owner: { scope: "service", lineageId: webLineage }, key: "PLOYZ_PUBLIC_DOMAIN" },
      ] }));
    }
    const web = intent.services.find((service) => service.lineageId === webLineage);
    if (web) web.config.managedHostnames = [{ prefix: "web-shop", targetPort: null }];
    await applyProduction(intent);
    // production itself used cache live from elsewhere: its attempt holds cache's address already at its owner.
    await harness.db.update(schema.environmentDeployment).set({ variableProducers: [
      ...compileSavedEnvironmentIntent({ environmentId: productionId, intent }).variableProducers,
      { ownerScope: "service", ownerId: id(9), ownerLineageId: cacheLineage, key: "PLOYZ_PRIVATE_DOMAIN", value: { kind: "literal", value: "cache.shop-root.internal" } },
    ] }).where(eq(schema.environmentDeployment.environmentId, productionId));
    await harness.pool.query(`insert into organization_cluster_domain (organization_id, endpoint, name, encrypted_token, reserved_at, lease_renewed_at)
      values ('${organizationId}', 'https://dns.example', 'acme.ployz.dev', '${JSON.stringify(encryption.encrypt("token"))}', now(), now())`);
    const savedStateSnapshotId = await saved(branchId, { ...branch(), services: branch().services.map((service) => ({ ...service, variables: [
      variable(id(10), "DB_URL", ref(dbLineage, "PLOYZ_PUBLIC_DOMAIN")),
      variable(id(11), "DB_SEES_CACHE", ref(dbLineage, "CACHE_ADDR")),
      variable(id(14), "DB_SEES_WEB_URL", ref(dbLineage, "WEB_URL")),
    ] })) });
    const admitted = await harness.runTransaction(() => admitEnvironmentDeployment({
      environmentId: branchId, savedStateSnapshotId, triggerOrigin: { origin: "manual", actorId: userId }, message: null,
    }));
    const [row] = await harness.db.select().from(schema.environmentDeployment).where(eq(schema.environmentDeployment.id, admitted.id));
    const produced = (lineage: string, key: string) => row?.variableProducers?.find((producer) =>
      producer.ownerLineageId === lineage && producer.key === key)?.value;
    expect(produced(dbLineage, "PLOYZ_PUBLIC_DOMAIN")).toEqual({ kind: "literal", value: "db-shop.acme.ployz.dev" });
    expect(produced(`shop-production::${cacheLineage}`, "PLOYZ_PRIVATE_DOMAIN")).toEqual({ kind: "literal", value: "cache.shop-root.internal" });
    // production's web, not the Branch's own copy of web.
    expect(produced(`shop-production::${webLineage}`, "PLOYZ_PUBLIC_DOMAIN")).toEqual({ kind: "literal", value: "web-shop.acme.ployz.dev" });
    expect(row?.missingLiveValues).toEqual([]);
  });
});
