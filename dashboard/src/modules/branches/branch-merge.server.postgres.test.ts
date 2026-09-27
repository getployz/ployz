import { randomUUID } from "node:crypto";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { and, eq, inArray } from "drizzle-orm";
import { Effect } from "effect";
import { Inngest } from "inngest";
import type { MachineId, ProjectName } from "@ployz/sdk";
import { branchChanges, parseServiceConfig, type ServiceSource } from "@ployz/sdk/config";
import * as schema from "#/db/schema";
import { asTestDouble } from "#/lib/test-double";
import { createManualEnvironmentDeployment } from "#/modules/deployments/deployment-command.server";
import { listLatestOrganizationEnvironmentChangeStates } from "#/modules/deployments/deployment-operations.server";
import type { SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { loadCurrentEnvironmentSnapshotProjection, loadCurrentEnvironmentState, loadEnvironmentDocument, writeEnvironmentDocument } from "#/modules/environment-design/working-state-repository.server";
import { fingerprintReviewedEnvironmentWorkingStateSync } from "#/modules/environment-design/working-state-fingerprint.server";
import { InngestClient } from "#/modules/inngest/client";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import { makeSecretEncryption, SecretEncryption } from "#/utils/encrypted-secret.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";
import { mergeBranch } from "./branch-merge.server";
import { createBranch } from "./branch-operations.server";
import { branchHostnameSuffix } from "./branch-plan";
import { mergeInput } from "./branch-review";
import type { MergePick } from "./branch-schemas";

const organizationId = "00000000-0000-4000-8000-000000000a01";
const userId = "00000000-0000-4000-8000-000000000a02";
const projectId = "00000000-0000-4000-8000-000000000a03";
const parentId = "00000000-0000-4000-8000-000000000a04";
const webLineage = "00000000-0000-4000-8000-000000000a11";
const webId = "00000000-0000-4000-8000-000000000a12";
const dbLineage = "00000000-0000-4000-8000-000000000a21";
const dbId = "00000000-0000-4000-8000-000000000a22";
const tokenId = "00000000-0000-4000-8000-000000000a31";
const workerLineage = "00000000-0000-4000-8000-000000000a41";
const encryption = makeSecretEncryption("test-encryption-secret");
const sealed = encryption.encrypt("parent-token");

function config(slug: string, image: string) {
  const source: ServiceSource = { version: 1, type: "image", image, credentials: { type: "none" } };
  const { env: _env, mounts: _mounts, ...parsed } = parseServiceConfig({
    version: 2, source, healthcheck: { type: "none" }, restartPolicy: "unless-stopped", privateDns: slug, managedHostnames: [],
  });
  return parsed;
}

/** production: web (a secret) and db, which web doesn't use, so a Branch of web leaves db out. */
const parentIntent = {
  version: 1, environmentSlug: "shop-production",
  services: [{
    id: webId, lineageId: webLineage, slug: "web", config: config("web", "web:1"), volumeAttachments: [],
    variables: [{ id: tokenId, key: "TOKEN", description: null, exported: false, valueFingerprint: encryption.sealedFingerprint("parent-token"), value: { kind: "secret", encryptedValue: null } }],
  }, { id: dbId, lineageId: dbLineage, slug: "db", config: config("db", "postgres:17"), volumeAttachments: [], variables: [] }],
  volumes: [],
};

// The runtime reports one volume per namespace it would destroy; "shop-unreachable" can't be reached.
const runtime = {
  cancel: () => Effect.void,
  open: () => Effect.succeed({
    status: "connected" as const,
    connected: asTestDouble<PloyzSession>()({
      dataLossIfProjectDestroyed: (namespace: ProjectName) => namespace === "shop-unreachable"
        ? Effect.fail(new Error("The runtime is unreachable."))
        : Effect.succeed({ data_loss: [{ kind: "docker_volume" as const, id: { machine_id: "a".repeat(32) as MachineId, name: `${namespace}-data` } }], unknown_machines: [] }),
    }),
  }),
};

describe("mergeBranch", () => {
  let harness: PostgresTestHarness;
  const inngest = new Inngest({ id: "merge-branch-test" });
  vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

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
        values ('${parentId}', '${projectId}', '${organizationId}', 'production', 'shop-production', '${JSON.stringify(parentIntent)}');
      insert into service_lineage (id, organization_id, project_id, canonical_name, canonical_slug) values
        ('${webLineage}', '${organizationId}', '${projectId}', 'Web', 'web'),
        ('${dbLineage}', '${organizationId}', '${projectId}', 'DB', 'db'),
        ('${workerLineage}', '${organizationId}', '${projectId}', 'Worker', 'worker');
      insert into service (id, project_id, environment_id, organization_id, lineage_id, name, policy) values
        ('${webId}', '${projectId}', '${parentId}', '${organizationId}', '${webLineage}', 'Storefront', '{"autoDeploy":false}'),
        ('${dbId}', '${projectId}', '${parentId}', '${organizationId}', '${dbLineage}', 'Postgres', '{"autoDeploy":false}');
      insert into variable (id, organization_id, environment_id, service_id) values ('${tokenId}', '${organizationId}', '${parentId}', '${webId}');
      insert into variable_secret (organization_id, environment_id, variable_id, encrypted_value)
        values ('${organizationId}', '${parentId}', '${tokenId}', '${JSON.stringify(sealed)}');
    `);
    await harness.db.insert(schema.member).values({ id: randomUUID(), userId, organizationId, role: "owner" });
  });

  const provide = <A, E, R>(operation: Effect.Effect<A, E, R>) => harness.runEffect(operation.pipe(
    Effect.provideService(SecretEncryption, encryption),
    Effect.provideService(InngestClient, inngest),
    Effect.provideService(OrganizationRuntime, runtime),
  ) as Effect.Effect<A, E, never>);

  const attemptsOf = (environmentId: string) =>
    harness.db.select().from(schema.environmentDeployment).where(eq(schema.environmentDeployment.environmentId, environmentId));
  const teardowns = () => harness.db.select().from(schema.teardownAttempt);
  const intentOf = async (environmentId: string) => (await provide(loadCurrentEnvironmentState(environmentId))).intent;

  /** Every queued attempt of the Environment finished and applied. */
  const settle = (environmentId: string) => harness.db.update(schema.environmentDeployment)
    .set({ status: "applied", finishedAt: new Date() })
    .where(and(eq(schema.environmentDeployment.environmentId, environmentId), eq(schema.environmentDeployment.status, "queued")));

  /** The Deploy button: publish and admit the Working State, then let it apply. */
  async function deploy(environmentId: string) {
    const [latest] = await harness.db.select().from(schema.environmentSavedStateSnapshot)
      .where(eq(schema.environmentSavedStateSnapshot.environmentId, environmentId))
      .orderBy(schema.environmentSavedStateSnapshot.createdAt).then((rows) => rows.slice(-1));
    const projection = await provide(loadCurrentEnvironmentSnapshotProjection(environmentId));
    await provide(createManualEnvironmentDeployment({
      environmentId, actorId: userId, message: null,
      review: {
        savedStateBasis: latest ? { kind: "saved_revision", savedStateSnapshotId: latest.id } : { kind: "no_saved_state" },
        workingStateFingerprint: fingerprintReviewedEnvironmentWorkingStateSync(projection),
        destructiveServiceIds: [], destructiveVolumeReviews: [],
      },
    }));
    await settle(environmentId);
  }

  const edit = (environmentId: string, change: (intent: SavedEnvironmentIntent) => void) => provide(Effect.gen(function* () {
    const { intent } = yield* loadCurrentEnvironmentState(environmentId);
    change(intent);
    return yield* writeEnvironmentDocument(yield* loadEnvironmentDocument(environmentId, true), intent);
  }));
  const setImage = (image: string) => (intent: SavedEnvironmentIntent) => {
    const web = intent.services.find((node) => node.lineageId === webLineage);
    if (web?.config.source.type === "image") web.config.source.image = image;
  };

  async function create(name: string, keep = false) {
    const { data } = await provide(createBranch({ userId }, {
      organizationSlug: "acme", parentEnvironmentId: parentId, name,
      focus: [webLineage], picks: { preset: "only" }, keep, deployNow: true, setupCommands: [],
    }));
    return data.environment;
  }

  /** The browser's path: the rows from Org Store rows (redacted Working States, the change-state projection's Applied State). */
  async function review(branchId: string) {
    const [branch] = await harness.db.select().from(schema.environmentBranch).where(eq(schema.environmentBranch.environmentId, branchId));
    const environments = await harness.db.select().from(schema.environment).where(inArray(schema.environment.id, [branchId, parentId]));
    const states = await provide(listLatestOrganizationEnvironmentChangeStates({ userId }, { organizationSlug: "acme" }));
    // The Parent's Applied State reaches the browser redacted: its secret as a fingerprint only.
    if (JSON.stringify(states).includes("ciphertext")) throw new Error("Sealed ciphertext reached the browser.");
    const own = environments.find((row) => row.id === branchId);
    const parent = environments.find((row) => row.id === parentId);
    if (!branch || !own || !parent) throw new Error("The Branch or its Parent is missing.");
    const changes = branchChanges(mergeInput({
      base: branch.base, kept: branch.kept, branch: own.intent, parent: parent.intent,
      parentApplied: states.find((state) => state.environmentId === parentId)?.applied.intent ?? null,
      hostnames: { branch: branchHostnameSuffix("shop", own.namespace, true), parent: "" },
    }));
    const rows = changes.rows.flatMap((row) => row.role === "move" ? [row] : []);
    return { rows, review: changes.review, revision: parent.revision };
  }

  const merge = (branchId: string, seen: Awaited<ReturnType<typeof review>>, picks: MergePick[], thenClose = true) =>
    provide(mergeBranch({ userId }, {
      organizationSlug: "acme", branchEnvironmentId: branchId, destinationRevision: seen.revision, review: seen.review, picks, thenClose,
    }));
  const defaults = (rows: Awaited<ReturnType<typeof review>>["rows"]): MergePick[] =>
    rows.map((row) => row.choice ? { key: row.key, option: row.choice.default, value: "" } : { key: row.key, value: "" });

  it("stages the picked changes in the Destination, seals a new secret, deletes nothing, deploys nothing, and closes the Branch", async () => {
    await deploy(parentId);
    const branch = await create("fix-web");
    await settle(branch.id);
    // The Branch changes web's image, adds a variable and a secret, and gains a new worker service; then deploys.
    const workerId = randomUUID();
    await harness.pool.query(`insert into service (id, project_id, environment_id, organization_id, lineage_id, name)
      values ('${workerId}', '${projectId}', '${branch.id}', '${organizationId}', '${workerLineage}', 'Worker')`);
    await edit(branch.id, (intent) => {
      setImage("web:2")(intent);
      const web = intent.services.find((node) => node.lineageId === webLineage);
      web?.variables.push(
        { id: randomUUID(), key: "FEATURE", description: null, exported: false, valueFingerprint: "fp-feature", value: { kind: "literal", value: "on" } },
        { id: randomUUID(), key: "API_KEY", description: null, exported: false, valueFingerprint: encryption.sealedFingerprint("test-key"), value: { kind: "secret", encryptedValue: encryption.encrypt("test-key") } },
      );
      intent.services.push({ id: workerId, lineageId: workerLineage, slug: "worker", config: config("worker", "worker:1"), variables: [], volumeAttachments: [] });
    });
    await deploy(branch.id);
    // production also changed web's image since branching: a conflict.
    await edit(parentId, setImage("web:3"));

    const seen = await review(branch.id);
    const image = seen.rows.find((row) => row.key === `${webLineage}:source.image`);
    expect(image).toMatchObject({ conflict: true, into: "web:3", from: "web:2" });
    // A new secret asks for production's value by default.
    const apiKey = seen.rows.find((row) => row.key === `${webLineage}:variables.API_KEY`);
    expect(apiKey?.choice).toMatchObject({ default: "new", secret: true });
    // Left empty, it would never land while Then close tears the Branch down: refused, and nothing changes.
    const empty = await merge(branch.id, seen, defaults(seen.rows)).then(() => null, (error: Error) => error);
    expect(empty).toMatchObject({ _tag: "Validation", message: "Enter a new value for API_KEY." });
    expect(await teardowns()).toEqual([]);
    const picks = defaults(seen.rows).map((pick) => pick.key === apiKey?.key ? { ...pick, value: "production-key" } : pick);
    const { data } = await merge(branch.id, seen, picks);
    expect(data.closed).toBe(true);

    // production's Working State has the Branch's changes, its own db, and the worker under a fresh id.
    const production = await intentOf(parentId);
    const web = production.services.find((node) => node.lineageId === webLineage);
    expect(web?.config.source).toMatchObject({ image: "web:2" });
    expect(web?.variables.find((variable) => variable.key === "FEATURE")?.value).toEqual({ kind: "literal", value: "on" });
    const key = web?.variables.find((variable) => variable.key === "API_KEY");
    expect(key?.valueFingerprint).toBe(encryption.sealedFingerprint("production-key"));
    expect(key?.value.kind === "secret" && key.value.encryptedValue ? encryption.decrypt(key.value.encryptedValue) : null).toBe("production-key");
    expect(production.services.map((node) => node.id)).toEqual(expect.arrayContaining([webId, dbId]));
    const worker = production.services.find((node) => node.lineageId === workerLineage);
    expect(worker?.id).not.toBe(workerId);
    const identities = await harness.db.select().from(schema.service).where(eq(schema.service.environmentId, parentId));
    expect(identities).toEqual(expect.arrayContaining([expect.objectContaining({ id: worker?.id, lineageId: workerLineage, name: "Worker" })]));
    const introductions = await harness.db.select().from(schema.environmentNodeIntroduction).where(eq(schema.environmentNodeIntroduction.environmentId, parentId));
    expect(introductions.map((row) => row.nodeId)).toContain(worker?.id);

    // Nothing is published or deployed in production; the Branch closes after commit.
    expect(await attemptsOf(parentId)).toHaveLength(1);
    expect(await harness.db.select().from(schema.environmentSavedStateSnapshot)
      .where(eq(schema.environmentSavedStateSnapshot.environmentId, parentId))).toHaveLength(1);
    expect(await teardowns()).toEqual([expect.objectContaining({ scope: "environment", environmentId: branch.id })]);
  });

  it("refuses an active attempt, undeployed changes, a stale review or revision, and then closes nothing", async () => {
    const branch = await create("fix-web");
    const refusal = async (seen: Awaited<ReturnType<typeof review>>) =>
      (await provide(Effect.flip(mergeBranch({ userId }, {
        organizationSlug: "acme", branchEnvironmentId: branch.id, destinationRevision: seen.revision, review: seen.review,
        picks: defaults(seen.rows), thenClose: true,
      })))).message;

    await edit(branch.id, setImage("web:2"));
    expect(await refusal(await review(branch.id))).toMatch(/still running/);
    await settle(branch.id);
    expect(await refusal(await review(branch.id))).toMatch(/aren't deployed/);
    await deploy(branch.id);
    const seen = await review(branch.id);
    expect(await refusal({ ...seen, review: "stale" })).toBe("Changes moved after this review. Review them again.");
    expect(await refusal({ ...seen, revision: randomUUID() })).toMatch(/Working State changed/);
    // The Branch moved after the review.
    await edit(branch.id, setImage("web:4"));
    await deploy(branch.id);
    expect(await refusal(seen)).toBe("Changes moved after this review. Review them again.");

    expect((await intentOf(parentId)).services.find((node) => node.lineageId === webLineage)?.config.source).toMatchObject({ image: "web:1" });
    expect(await teardowns()).toEqual([]);
  });

  it("keeps a Kept Branch, advancing its base; a close that can't start leaves the Merge standing", async () => {
    const kept = await create("staging", true);
    await settle(kept.id);
    await edit(kept.id, setImage("web:2"));
    await deploy(kept.id);
    const seen = await review(kept.id);
    expect((await merge(kept.id, seen, defaults(seen.rows))).data.closed).toBe(false);
    expect((await review(kept.id)).rows).toEqual([]);

    const unreachable = await create("unreachable");
    await settle(unreachable.id);
    await edit(unreachable.id, setImage("web:5"));
    await deploy(unreachable.id);
    const next = await review(unreachable.id);
    expect((await merge(unreachable.id, next, defaults(next.rows))).data.closed).toBe(false);
    expect((await intentOf(parentId)).services.find((node) => node.lineageId === webLineage)?.config.source).toMatchObject({ image: "web:5" });
    expect(await teardowns()).toEqual([]);
  });

  it("leaves an unticked variable of a new service in the Branch, and brings registry credentials to an existing service", async () => {
    await deploy(parentId);
    const branch = await create("creds");
    await settle(branch.id);
    const workerId = randomUUID();
    await harness.pool.query(`insert into service (id, project_id, environment_id, organization_id, lineage_id, name)
      values ('${workerId}', '${projectId}', '${branch.id}', '${organizationId}', '${workerLineage}', 'Worker')`);
    const branchWebId = (await intentOf(branch.id)).services.find((node) => node.lineageId === webLineage)?.id ?? "";
    await harness.db.insert(schema.serviceRegistryCredential).values({
      organizationId, serviceId: branchWebId, encryptedRegistryUsername: encryption.encrypt("robot"), encryptedRegistrySecret: encryption.encrypt("hunter2"),
    });
    await edit(branch.id, (intent) => {
      const web = intent.services.find((node) => node.lineageId === webLineage);
      if (web?.config.source.type === "image") web.config.source.credentials = { type: "configured", credentialId: web.id };
      const variable = (key: string) => ({ id: randomUUID(), key, description: null, exported: false, valueFingerprint: `fp-${key}`, value: { kind: "literal" as const, value: key } });
      intent.services.push({ id: workerId, lineageId: workerLineage, slug: "worker", config: config("worker", "worker:1"), variables: [variable("KEEP"), variable("TEST_ONLY")], volumeAttachments: [] });
    });
    await deploy(branch.id);

    const seen = await review(branch.id);
    const picks = defaults(seen.rows).filter((pick) => pick.key !== `${workerLineage}:variables.TEST_ONLY`);
    expect(picks.map((pick) => pick.key)).toEqual(expect.arrayContaining([`${webLineage}:source.credentials`, `${workerLineage}:variables.KEEP`]));
    await merge(branch.id, seen, picks, false);

    const production = await intentOf(parentId);
    expect(production.services.find((node) => node.lineageId === workerLineage)?.variables.map((variable) => variable.key)).toEqual(["KEEP"]);
    expect(production.services.find((node) => node.id === webId)?.config.source).toMatchObject({ credentials: { type: "configured", credentialId: webId } });
    const [credential] = await harness.db.select().from(schema.serviceRegistryCredential).where(eq(schema.serviceRegistryCredential.serviceId, webId));
    expect(credential?.encryptedRegistrySecret ? encryption.decrypt(credential.encryptedRegistrySecret) : null).toBe("hunter2");
    const [identity] = await harness.db.select().from(schema.service).where(eq(schema.service.id, webId));
    expect(identity?.hasRegistryCredential).toBe(true);
  });
});
