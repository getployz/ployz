import { randomUUID } from "node:crypto";
import { afterAll, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { branchChanges, parseServiceConfig } from "@ployz/sdk/config";
import * as schema from "#/db/schema";
import { loadCurrentEnvironmentState } from "#/modules/environment-design/working-state-repository.server";
import { compileSavedEnvironmentIntent, type SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { loadAppliedIntent } from "#/modules/environment-design/saved-state-operations.server";
import { loadEnvironmentSnapshotProjection } from "#/modules/deployments/environment-state.repository.server";
import { InngestClient } from "#/modules/inngest/client";
import type { Database, ReportingDatabase } from "#/server/database.server";
import { makeSecretEncryption, SecretEncryption } from "#/utils/encrypted-secret.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";
import { createBranch } from "./branch-operations.server";
import { makeOwnCopy, updateBranch } from "./branch-update.server";
import { updateInput } from "./branch-review";

const organizationId = "00000000-0000-4000-8000-000000000501";
const userId = "00000000-0000-4000-8000-000000000502";
const projectId = "00000000-0000-4000-8000-000000000503";
const parentId = "00000000-0000-4000-8000-000000000504";
const webLineage = "00000000-0000-4000-8000-000000000511";
const webId = "00000000-0000-4000-8000-000000000512";
const dbLineage = "00000000-0000-4000-8000-000000000521";
const dbId = "00000000-0000-4000-8000-000000000522";
const dataLineage = "00000000-0000-4000-8000-000000000531";
const dataId = "00000000-0000-4000-8000-000000000532";
const tokenId = "00000000-0000-4000-8000-000000000541";
const urlId = "00000000-0000-4000-8000-000000000542";
const encrypted = { version: 1 as const, iv: "iv", tag: "tag", ciphertext: "parent-cipher" };
const policy = { autoDeploy: false };

const config = (slug: string, image: string) => {
  const { env: _env, mounts: _mounts, ...parsed } = parseServiceConfig({
    version: 2, source: { version: 1, type: "image", image, credentials: { type: "none" } },
    healthcheck: { type: "none" }, restartPolicy: "unless-stopped", privateDns: slug, managedHostnames: [],
  });
  return parsed;
};

/** production: web (a secret, a reference to db) uses db, which mounts data. */
const parentIntent = (webImage = "web:1"): SavedEnvironmentIntent => ({
  version: 1, environmentSlug: "shop-production",
  services: [{
    id: webId, lineageId: webLineage, slug: "web", config: config("web", webImage),
    variables: [
      { id: tokenId, key: "TOKEN", description: null, exported: false, valueFingerprint: "fp-token", value: { kind: "secret", encryptedValue: null } },
      { id: urlId, key: "DB_URL", description: null, exported: false, valueFingerprint: "fp-url",
        value: { kind: "template", parts: [{ kind: "ref", owner: { scope: "service", lineageId: dbLineage }, key: "PLOYZ_PRIVATE_DOMAIN" }] } },
    ],
    volumeAttachments: [],
  }, {
    id: dbId, lineageId: dbLineage, slug: "db", config: config("db", "postgres:17"),
    variables: [], volumeAttachments: [{ volumeResourceId: dataId, mountPath: "/var/lib/postgresql" }],
  }],
  volumes: [{ resourceId: dataId, resourceLineageId: dataLineage, name: "data" }],
});

describe("updateBranch", () => {
  let harness: PostgresTestHarness;
  const inngest = new Inngest({ id: "update-branch-test" });
  let clock = 1_000;

  beforeAll(async () => {
    harness = await startPostgresTestHarness();
  }, 60_000);
  afterAll(async () => {
    await harness?.stop();
  });

  beforeEach(async () => {
    await harness.pool.query(`
      truncate table organization, "user" cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
      insert into "user" (id, email, name) values ('${userId}', 'owner@example.com', 'Owner');
      insert into project (id, organization_id, name, slug) values ('${projectId}', '${organizationId}', 'Shop', 'shop');
      insert into environment (id, project_id, organization_id, name, namespace, intent)
        values ('${parentId}', '${projectId}', '${organizationId}', 'production', 'shop-production', '${JSON.stringify(parentIntent())}');
      insert into service_lineage (id, organization_id, project_id, canonical_name, canonical_slug) values
        ('${webLineage}', '${organizationId}', '${projectId}', 'Web', 'web'),
        ('${dbLineage}', '${organizationId}', '${projectId}', 'DB', 'db');
      insert into service (id, project_id, environment_id, organization_id, lineage_id, name, policy, has_registry_credential) values
        ('${webId}', '${projectId}', '${parentId}', '${organizationId}', '${webLineage}', 'Storefront', '${JSON.stringify(policy)}', false),
        ('${dbId}', '${projectId}', '${parentId}', '${organizationId}', '${dbLineage}', 'Postgres', '${JSON.stringify(policy)}', false);
      insert into resource_lineage (id, organization_id, project_id, canonical_name, canonical_slug)
        values ('${dataLineage}', '${organizationId}', '${projectId}', 'data', 'data');
      insert into environment_resource (id, organization_id, project_id, environment_id, lineage_id, implementation_type)
        values ('${dataId}', '${organizationId}', '${projectId}', '${parentId}', '${dataLineage}', 'volume');
      insert into variable (id, organization_id, environment_id, service_id) values
        ('${tokenId}', '${organizationId}', '${parentId}', '${webId}'),
        ('${urlId}', '${organizationId}', '${parentId}', '${webId}');
      insert into variable_secret (organization_id, environment_id, variable_id, encrypted_value)
        values ('${organizationId}', '${parentId}', '${tokenId}', '${JSON.stringify(encrypted)}');
    `);
    await harness.db.insert(schema.member).values({ id: randomUUID(), userId, organizationId, role: "owner" });
  });

  const provide = <A, E>(effect: Effect.Effect<A, E, Database | ReportingDatabase | SecretEncryption | InngestClient>) => harness.runEffect(effect.pipe(
    Effect.provideService(SecretEncryption, makeSecretEncryption("test-encryption-secret")),
    Effect.provideService(InngestClient, inngest),
  ));

  /** Deploy the Environment's Working State by hand: a Saved revision, an attempt and its node snapshots. */
  async function deploy(environmentId: string, status: "applied" | "queued" = "applied") {
    const { intent } = await provide(loadCurrentEnvironmentState(environmentId));
    clock += 1_000;
    const [saved] = await harness.db.insert(schema.environmentSavedStateSnapshot).values({
      organizationId, environmentId, actorId: userId, intent, volumeDeletionAuthorizations: [], createdAt: new Date(clock),
    }).returning();
    const [attempt] = await harness.db.insert(schema.environmentDeployment).values({
      organizationId, environmentId, savedStateSnapshotId: saved?.id ?? "", status,
      triggerOrigin: { origin: "manual", actorId: userId }, createdAt: new Date(clock),
    }).returning();
    for (const node of compileSavedEnvironmentIntent({ environmentId, intent }).nodeSnapshots) {
      await harness.db.insert(schema.environmentNodeConfigSnapshot).values({ ...node, organizationId, environmentId, environmentDeploymentId: attempt?.id ?? "" });
    }
    return attempt;
  }

  async function branchOfWeb() {
    await deploy(parentId);
    const { data } = await provide(createBranch({ userId }, {
      organizationSlug: "acme", parentEnvironmentId: parentId, name: "fix-web",
      focus: [webLineage], picks: { preset: "only" }, keep: false, deployNow: false, setupCommands: [],
    }));
    await deploy(data.environment.id);
    return data.environment.id;
  }

  const documentOf = async (environmentId: string) =>
    (await harness.db.select().from(schema.environment).where(eq(schema.environment.id, environmentId)))[0];
  const update = async (environmentId: string, ownCopyOf?: string) => {
    const at = { organizationSlug: "acme", environmentId, revision: (await documentOf(environmentId))?.revision ?? "" };
    return provide(ownCopyOf ? makeOwnCopy({ userId }, { ...at, lineageId: ownCopyOf }) : updateBranch({ userId }, at));
  };
  const failure = (promise: Promise<unknown>) => promise.then(() => null, (error: Error) => error);

  /** The Parent's deployed changes the Branch lacks, as the review page computes them. */
  async function updateRows(branchId: string) {
    const [row] = await harness.db.select().from(schema.environmentBranch).where(eq(schema.environmentBranch.environmentId, branchId));
    const parentApplied = await provide(Effect.gen(function* () {
      return yield* loadAppliedIntent(parentId, "shop-production", yield* loadEnvironmentSnapshotProjection({ kind: "environment", environmentId: parentId }));
    }));
    const branch = await documentOf(branchId);
    if (!row || !branch) throw new Error("Branch missing.");
    return branchChanges(updateInput({
      base: row.base, kept: false, branch: branch.intent, parent: parentApplied, parentApplied,
      hostnames: { branch: "-fix-web", parent: "" },
    }, parentApplied)).rows.filter((change) => change.role === "move");
  }

  it("stages the Parent's deployed changes, advances the base, and has nothing left once the Branch deploys", async () => {
    const branchId = await branchOfWeb();
    // production deploys web:2 with a new variable.
    const next = parentIntent("web:2");
    next.services[0]?.variables.push({ id: randomUUID(), key: "NEW", description: null, exported: false, valueFingerprint: "fp-new", value: { kind: "literal", value: "1" } });
    await harness.db.update(schema.environment).set({ intent: next }).where(eq(schema.environment.id, parentId));
    await deploy(parentId);
    expect((await updateRows(branchId)).map((change) => change.key).sort()).toEqual([`${webLineage}:source.image`, `${webLineage}:variables.NEW`].sort());

    await update(branchId);
    const web = (await documentOf(branchId))?.intent.services.find((node) => node.lineageId === webLineage);
    expect(web?.config.source).toMatchObject({ image: "web:2" });
    expect(web?.variables.find((variable) => variable.key === "NEW")?.value).toEqual({ kind: "literal", value: "1" });
    const [row] = await harness.db.select().from(schema.environmentBranch).where(eq(schema.environmentBranch.environmentId, branchId));
    expect(row?.base.services.find((node) => node.lineageId === webLineage)?.config.source).toMatchObject({ image: "web:2" });
    expect(await updateRows(branchId)).toEqual([]);

    // Staged until the Branch deploys; then Update has nothing to take.
    expect(await failure(update(branchId))).toMatchObject({ _tag: "Conflict", message: expect.stringContaining("aren't deployed") });
    await deploy(branchId);
    expect(await failure(update(branchId))).toMatchObject({ _tag: "Conflict", message: "Nothing new in production." });
  });

  it("is refused while the Branch has an active attempt", async () => {
    const branchId = await branchOfWeb();
    await deploy(branchId, "queued");
    expect(await failure(update(branchId))).toMatchObject({ _tag: "Conflict", message: expect.stringContaining("still running") });
  });

  it("turns a Live Node into an Own Copy, with an empty copy of the Volume it mounts", async () => {
    const branchId = await branchOfWeb();
    expect((await documentOf(branchId))?.intent.services.map((node) => node.lineageId)).toEqual([webLineage]);

    await update(branchId, dbLineage);
    const intent = (await documentOf(branchId))?.intent;
    const db = intent?.services.find((node) => node.lineageId === dbLineage);
    expect(db?.id).not.toBe(dbId);
    expect(intent?.volumes).toEqual([expect.objectContaining({ resourceLineageId: dataLineage })]);
    expect(intent?.volumes[0]?.resourceId).not.toBe(dataId);
    expect(db?.volumeAttachments).toEqual([{ volumeResourceId: intent?.volumes[0]?.resourceId, mountPath: "/var/lib/postgresql" }]);
    const introductions = await harness.db.select().from(schema.environmentNodeIntroduction)
      .where(eq(schema.environmentNodeIntroduction.environmentId, branchId));
    expect(introductions.map((row) => row.nodeId)).toEqual(expect.arrayContaining([db?.id, intent?.volumes[0]?.resourceId]));
    const [row] = await harness.db.select().from(schema.environmentBranch).where(eq(schema.environmentBranch.environmentId, branchId));
    expect(row?.base.services.map((node) => node.lineageId).sort()).toEqual([dbLineage, webLineage].sort());

    // After it deploys, nothing is left to update.
    await deploy(branchId);
    expect(await updateRows(branchId)).toEqual([]);
  });
});
