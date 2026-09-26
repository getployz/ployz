import { InngestTestEngine, mockCtx } from "@inngest/test";
import type { BuildOptions, BuildOutcome, BuildReceipt, BuildReceipts, Client, PreparationInput, PreparedDeploy } from "@ployz/sdk";
import { eq } from "drizzle-orm";
import { Effect, Layer, Schema } from "effect";
import { Inngest } from "inngest";
import { gzipSync } from "node:zlib";
import { Header } from "tar";
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { createDefaultServiceHealthcheck, createDefaultServiceRestartPolicy, createGitServiceSource, projectServiceDeploymentConfig } from "#/modules/environment-design/services";
import { GithubApi } from "#/modules/github/github-observation.api";
import { InngestClient } from "#/modules/inngest/client";
import { makeOrganizationRuntimeLayer } from "#/modules/runtime/organization-runtime.server";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";
import { runtimeWatchMachineFixture } from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { noPairingChanges } from "#/test/organization-runtime";
import { loadDeploymentBuildLog } from "./deployment-events.server";
import { requestDeploymentCancellation } from "./runtime-cancellation.repository.server";
import { admitEnvironmentDeployment } from "./admission.server";
import { dispatchEnvironmentDeployment, dispatchPendingDeployment, dispatchStrandedPendingDeployments } from "./runtime-lifecycle.repository.server";
import * as schema from "#/db/schema";
import {
  type PostgresTestHarness,
  startPostgresTestHarness,
} from "#/test/postgres";
import { Database } from "#/server/database.server";
import { makeInngestEffectRunner, type runInngestEffect } from "#/server/run.server";
import {
  makeSecretEncryption,
  SecretEncryption,
} from "#/utils/encrypted-secret.server";
import {
  createMarkCancelledRowBackedWorkflow,
  createProcessEnvironmentDeployment,
} from "./environment-deployment.inngest";

const organizationId = "00000000-0000-4000-8000-000000000701";
const userId = "00000000-0000-4000-8000-000000000702";
const projectId = "00000000-0000-4000-8000-000000000703";
const environmentId = "00000000-0000-4000-8000-000000000704";
const savedId = "00000000-0000-4000-8000-000000000705";
const activeDeploymentId = "00000000-0000-4000-8000-000000000706";
const targetDeploymentId = "00000000-0000-4000-8000-000000000707";
const targetRunId = "deployment-smoke-run";
const encryption = makeSecretEncryption("test-encryption-secret");

const emptySavedIntent = {
  version: 1 as const,
  environmentSlug: "production",
  services: [],
  volumes: [],
};

describe("deployment Inngest durable smoke", () => {
  let harness: PostgresTestHarness;
  /**
   * Every effect an Inngest run started, with its after-commit dispatches. A test can end while a replayed step still runs;
   * its transaction would then hold locks the next test's truncate waits on.
   */
  const inFlight = new Set<Promise<unknown>>();
  function tracked<A>(run: Promise<A>) {
    inFlight.add(run);
    void run.then(() => inFlight.delete(run), () => inFlight.delete(run));
    return run;
  }

  beforeAll(async () => {
    harness = await startPostgresTestHarness();
  }, 60_000);

  afterAll(async () => {
    await harness?.stop();
  });

  afterEach(async () => {
    while (inFlight.size > 0) await Promise.allSettled([...inFlight]);
  });

  beforeEach(async () => {
    await harness.pool.query(`
      truncate table environment_saved_state_snapshot, environment, project,
        "user", organization cascade;
      insert into organization (id, name, slug)
      values ('${organizationId}', 'Durable smoke', 'durable-smoke');
      insert into "user" (id, email, name)
      values ('${userId}', 'owner@example.com', 'Owner');
      insert into project (id, organization_id, name, slug)
      values ('${projectId}', '${organizationId}', 'Cloud', 'cloud');
      insert into environment (
        id, project_id, organization_id, name, namespace, intent
      ) values (
        '${environmentId}', '${projectId}', '${organizationId}',
        'Production', 'production', '{"version":1,"environmentSlug":"production","services":[],"volumes":[]}'
      );
    `);
    await harness.db.insert(schema.environmentSavedStateSnapshot).values({
      id: savedId,
      organizationId,
      environmentId,
      actorId: userId,
      intent: emptySavedIntent,
      volumeDeletionAuthorizations: [],
    });
    await harness.db.insert(schema.environmentDeployment).values([
      {
        id: activeDeploymentId,
        organizationId,
        environmentId,
        savedStateSnapshotId: savedId,
        triggerOrigin: { origin: "manual", actorId: userId },
        dispatchRequestedAt: new Date(),
        status: "planning",
        inngestRunId: "active-deployment-run",
      },
      {
        id: targetDeploymentId,
        organizationId,
        environmentId,
        savedStateSnapshotId: savedId,
        triggerOrigin: { origin: "manual", actorId: userId },
        dispatchRequestedAt: new Date(),
      },
    ]);
  });

  it("settles cancellation between planning and runtime execution in PostgreSQL", async () => {
    const runEffect = makeInngestEffectRunner(
      <A, E>(operation: Effect.Effect<A, E, Database | SecretEncryption>) =>
        tracked(harness.runEffect(
          operation.pipe(Effect.provideService(SecretEncryption, encryption)),
        )),
    ) as typeof runInngestEffect;
    const inngest = new Inngest({ id: "durable-smoke" });
    await harness.db.update(schema.environmentDeployment).set({ status: "applied", finishedAt: new Date() })
      .where(eq(schema.environmentDeployment.id, activeDeploymentId));

    const resumed = await new InngestTestEngine({
      function: createProcessEnvironmentDeployment(inngest, runEffect),
      events: [
        {
          name: "environment/deploy.requested",
          data: {
            environmentDeploymentId: targetDeploymentId,
            environmentId,
          },
        },
      ],
      transformCtx: (context) => ({
        ...mockCtx(context),
        runId: targetRunId,
      }),
    }).executeStep("mark-deployment-planning");

    expect(resumed.result).toEqual({ state: "started" });
    const [planning] = await harness.db
      .select({
        status: schema.environmentDeployment.status,
        inngestRunId: schema.environmentDeployment.inngestRunId,
      })
      .from(schema.environmentDeployment)
      .where(eq(schema.environmentDeployment.id, targetDeploymentId));
    expect(planning).toEqual({ status: "planning", inngestRunId: targetRunId });

    const cancellation = await new InngestTestEngine({
      function: createMarkCancelledRowBackedWorkflow(inngest, runEffect),
      events: [
        {
          name: "inngest/function.cancelled",
          data: {
            function_id: "process-environment-deployment",
            run_id: targetRunId,
          },
        },
      ],
    }).execute();

    expect(cancellation.error).toBeUndefined();
    expect(cancellation.result).toEqual({
      functionId: "process-environment-deployment",
      runId: targetRunId,
      marked: true,
    });
    const [terminal] = await harness.db
      .select({
        status: schema.environmentDeployment.status,
        inngestRunId: schema.environmentDeployment.inngestRunId,
        cancellationRequestedAt:
          schema.environmentDeployment.cancellationRequestedAt,
        finishedAt: schema.environmentDeployment.finishedAt,
      })
      .from(schema.environmentDeployment)
      .where(eq(schema.environmentDeployment.id, targetDeploymentId));
    expect(terminal).toEqual({
      status: "cancelled",
      inngestRunId: targetRunId,
      cancellationRequestedAt: expect.any(Date),
      finishedAt: expect.any(Date),
    });
  });

  describe("Image Builds fanned out at admission", () => {
    const apiId = "00000000-0000-4000-8000-000000000711";
    const webId = "00000000-0000-4000-8000-000000000712";
    const retryDeploymentId = "00000000-0000-4000-8000-000000000713";
    const machine = runtimeWatchMachineFixture("a".repeat(32), "builder");
    const git = (privateDns: string) => projectServiceDeploymentConfig({
      source: createGitServiceSource({ repository: "owner/repo", repositoryId: 42, access: { type: "public" } }),
      privateDns, preDeployCommand: null, startCommand: null,
      healthcheck: createDefaultServiceHealthcheck(), restartPolicy: createDefaultServiceRestartPolicy(),
    });
    const receiptFor = (image: string): BuildReceipt => ({
      fingerprint: (image === "api" ? "b" : "d").repeat(64), machine_id: machine.id,
      image: { reference: `sha256:${"c".repeat(64)}`, tags: [], platforms: ["linux/amd64"], location: "unix:///var/run/docker.sock" },
    });
    const header = new Header({ path: "root/Dockerfile", size: 0, mode: 0o644, type: "File" });
    header.encode();
    const archive = gzipSync(Buffer.concat([Buffer.from(header.block ?? Buffer.alloc(512)), Buffer.alloc(1024)]));

    /** Cloud's Inngest client: records which attempts were dispatched. */
    const client = new Inngest({ id: "image-build-smoke" });
    let dispatched: string[] = [];
    vi.spyOn(client, "send").mockImplementation(async (payload) => {
      for (const event of [payload].flat()) {
        if (event.name === "environment/deploy.requested") dispatched.push(String(event.data?.["environmentDeploymentId"]));
      }
      return { ids: [] };
    });
    /** The Ployz SDK faked at its Context boundary: what each Image Build and the deploy step asked of it. */
    type Fake = {
      builds: { image: string; snapshots: number; hint: boolean; index: number | undefined }[];
      failImage: string | null;
      /** Holds each build until its signal aborts. */
      hold: boolean;
      prepared: BuildReceipts[];
    };
    function fakeClient(fake: Fake) {
      return asTestDouble<Client>()({
        build: (input: PreparationInput, options?: BuildOptions) => {
          const image = Object.keys(input.sources)[0] ?? "";
          const hint = input.build_receipts?.[image];
          fake.builds.push({ image, snapshots: input.deployment.snapshots.length, hint: hint !== undefined, index: input.build_index });
          // Like the SDK: a replayed step can reach the build after its cancellation watcher aborted,
          // and a held build waiting on that past abort would hang the harness teardown.
          options?.signal?.throwIfAborted();
          const aborted = new Promise<never>((_resolve, reject) => options?.signal?.addEventListener("abort",
            () => reject({ code: "cancelled", details: { preparation: { kind: "cancelled" } } }), { once: true }));
          void aborted.catch(() => undefined);
          const finished: Promise<BuildOutcome> = fake.hold ? aborted
            : image === fake.failImage ? Promise.reject({ code: "internal", details: { preparation: { kind: "failed", stage: "Building", message: "exit code: 2" } } })
            : Promise.resolve({ kind: "built", receipt: hint ?? receiptFor(image) });
          void finished.catch(() => undefined);
          return { abort: () => undefined, finished, async *[Symbol.asyncIterator]() {
            if (hint) return;
            yield { Selected: { machine, reason: { kind: "spread" as const }, rejections: [] } };
            yield { Build: { Stage: "Building" } };
            yield { Build: { Target: { name: image, outcome: null } } };
            await finished;
          } };
        },
        prepare: (input: PreparationInput) => {
          fake.prepared.push(input.build_receipts ?? {});
          const outcome = { type: "success" as const, completed: [] };
          const prepared = asTestDouble<PreparedDeploy>()({
            project_name: "production", operations: [], warnings: [], would_remove: [], preserved_volumes: [],
            buildReceipts: input.build_receipts ?? {}, pruneTargets: [], close: () => undefined,
            confirm: () => ({ abort: () => undefined, finished: Promise.resolve(outcome),
              async *[Symbol.asyncIterator]() { yield { type: "outcome" as const, outcome }; } }),
          });
          return { abort: () => undefined, finished: Promise.resolve(prepared), async *[Symbol.asyncIterator]() { yield* []; } };
        },
        close: async () => undefined,
      });
    }
    function runner(fake: Fake) {
      const runtime = makeOrganizationRuntimeLayer(() => Effect.succeed({ kind: "ready", generation: "grant", connections: [{ management: "ployz1:test" }] }), noPairingChanges)
        .pipe(Layer.provide(makePloyzLayer({ connect: async () => fakeClient(fake) })));
      return makeInngestEffectRunner(<A, E>(operation: Effect.Effect<A, E, never>) => tracked(harness.runEffect(operation.pipe(
        Effect.provide(runtime),
        Effect.provideService(GithubApi, {
          json: (request) => Schema.decodeUnknownEffect(request.schema)({ id: 42, full_name: "owner/repo", private: false }).pipe(Effect.orDie),
          archive: () => Effect.succeed(new Response(archive)),
        }),
        Effect.provideService(InngestClient, client),
        Effect.provideService(SecretEncryption, encryption),
      )))) as typeof runInngestEffect;
    }
    function engine(fake: Fake, deploymentId: string, runId: string) {
      return new InngestTestEngine({
        function: createProcessEnvironmentDeployment(new Inngest({ id: "image-build-smoke" }), runner(fake)),
        events: [{ name: "environment/deploy.requested", data: { environmentDeploymentId: deploymentId, environmentId } }],
        transformCtx: (context) => ({ ...mockCtx(context), runId }),
      });
    }
    const imageBuildRows = (deploymentId: string) => harness.db.select().from(schema.environmentDeploymentImageBuild)
      .where(eq(schema.environmentDeploymentImageBuild.deploymentId, deploymentId)).orderBy(schema.environmentDeploymentImageBuild.image);
    const attempt = async (deploymentId: string) => (await harness.db.select().from(schema.environmentDeployment)
      .where(eq(schema.environmentDeployment.id, deploymentId)))[0];
    async function freezeGitServices(deploymentId: string) {
      await harness.db.insert(schema.environmentNodeConfigSnapshot).values([["api", apiId], ["web", webId]].map(([name, id]) => ({
        organizationId, environmentDeploymentId: deploymentId, environmentId, nodeType: "service" as const,
        nodeId: id ?? "", nodeLineageId: id ?? "", config: git(name ?? ""),
      })));
      await harness.db.update(schema.environmentDeployment)
        .set({ sourcePins: { [apiId]: { commitSha: "a".repeat(40) }, [webId]: { commitSha: "a".repeat(40) } } })
        .where(eq(schema.environmentDeployment.id, deploymentId));
    }

    beforeEach(async () => {
      dispatched = [];
      for (const [id, name] of [[apiId, "api"], [webId, "web"]] as const) {
        await harness.db.insert(schema.serviceLineage).values({ id, organizationId, projectId, canonicalName: name, canonicalSlug: name });
        await harness.db.insert(schema.service).values({ id, organizationId, projectId, environmentId, lineageId: id, name });
      }
      await harness.db.insert(schema.environmentDeploymentSecret).values([activeDeploymentId, targetDeploymentId]
        .map((environmentDeploymentId) => ({ organizationId, environmentDeploymentId })));
      await freezeGitServices(targetDeploymentId);
    });

    it("builds a queued attempt's images while an earlier attempt deploys, one Git Service per build", async () => {
      await harness.db.update(schema.environmentDeployment).set({ status: "deploying" })
        .where(eq(schema.environmentDeployment.id, activeDeploymentId));
      const fake: Fake = { builds: [], failImage: null, hold: false, prepared: [] };
      const planning = await engine(fake, targetDeploymentId, targetRunId).executeStep("mark-deployment-planning");

      // The builds finished without the slot: the attempt is still queued behind the deploying one.
      expect(planning.result).toEqual({ state: "blocked" });
      expect((await attempt(targetDeploymentId))?.status).toBe("queued");
      // The test engine resumes once per parallel branch and may replay steps; Inngest itself runs each once.
      // Each build carries its position in the attempt, so the Engine spreads them across Servers.
      expect(new Set(fake.builds.map(({ image, snapshots, index }) => `${image}:${snapshots}:${index}`))).toEqual(new Set(["api:1:0", "web:1:1"]));
      const rows = await imageBuildRows(targetDeploymentId);
      const serverChoice = { machineName: machine.name, reason: { kind: "spread" } };
      expect(rows.map(({ image, status, machineId, serverChoice, inngestRunId }) => ({ image, status, machineId, serverChoice, inngestRunId }))).toEqual([
        { image: "api", status: "built", machineId: machine.id, serverChoice, inngestRunId: targetRunId },
        { image: "web", status: "built", machineId: machine.id, serverChoice, inngestRunId: targetRunId },
      ]);
      expect(JSON.parse(encryption.decrypt(rows[0]?.encryptedReceipt ?? encryption.encrypt("null")))).toEqual(receiptFor("api"));
      // Build Steps appear per Image Build before the attempt deploys.
      const log = await harness.runEffect(loadDeploymentBuildLog({ organizationId, deploymentId: targetDeploymentId, after: 0, limit: 50 }));
      expect(log.steps.map((step) => `${step.image}:${step.name}`)).toEqual(expect.arrayContaining(["api:api", "web:web"]));
      expect(log.imageBuilds).toEqual(expect.arrayContaining([{ image: "api", serverChoice, github: null, skips: [] }, { image: "web", serverChoice, github: null, skips: [] }]));
    });

    it("lets the others finish when one Image Build fails, and a retry rebuilds only the failed one", async () => {
      await harness.db.update(schema.environmentDeployment).set({ status: "applied", finishedAt: new Date() })
        .where(eq(schema.environmentDeployment.id, activeDeploymentId));
      const failing: Fake = { builds: [], failImage: "web", hold: false, prepared: [] };
      const failed = await engine(failing, targetDeploymentId, targetRunId).execute();
      expect(failed.error).toEqual(expect.objectContaining({ message: "Image Build failed: web." }));
      expect(await attempt(targetDeploymentId)).toMatchObject({ status: "failed", failureCode: "image_build_failed" });
      expect((await imageBuildRows(targetDeploymentId)).map(({ image, status }) => [image, status])).toEqual([["api", "built"], ["web", "failed"]]);
      expect(failing.prepared).toEqual([]);

      await harness.db.insert(schema.environmentDeployment).values({
        id: retryDeploymentId, organizationId, environmentId, savedStateSnapshotId: savedId, retryOfDeploymentId: targetDeploymentId,
        triggerOrigin: { origin: "manual", actorId: userId }, dispatchRequestedAt: new Date(),
      });
      await harness.db.insert(schema.environmentDeploymentSecret).values({ organizationId, environmentDeploymentId: retryDeploymentId });
      await freezeGitServices(retryDeploymentId);
      const retrying: Fake = { builds: [], failImage: null, hold: false, prepared: [] };
      // A replayed deploy step (test engine only) fails its own ownership check, so read the outcome from the row.
      await engine(retrying, retryDeploymentId, "retry-run").execute();
      await vi.waitFor(async () => expect(await attempt(retryDeploymentId)).toMatchObject({ status: "applied" }), { timeout: 10_000 });
      // api reused its receipt (the SDK reports it built without building); only web built again.
      expect(new Set(retrying.builds.filter(({ hint }) => !hint).map(({ image }) => image))).toEqual(new Set(["web"]));
      expect(retrying.prepared).toEqual([{ api: receiptFor("api"), web: receiptFor("web") }]);
      const log = await harness.runEffect(loadDeploymentBuildLog({ organizationId, deploymentId: retryDeploymentId, after: 0, limit: 50 }));
      expect(log.steps.filter((step) => step.image === "api").map((step) => step.name)).toEqual(["Reused image"]);
    });

    it("stops a queued attempt's Image Builds when it is cancelled, leaving no row active", async () => {
      await harness.db.update(schema.environmentDeployment).set({ status: "deploying" })
        .where(eq(schema.environmentDeployment.id, activeDeploymentId));
      const fake: Fake = { builds: [], failImage: null, hold: true, prepared: [] };
      const running = engine(fake, targetDeploymentId, targetRunId).execute();
      await vi.waitFor(() => expect(new Set(fake.builds.map(({ image }) => image)).size).toBe(2), { timeout: 10_000 });
      expect(await harness.runEffect(requestDeploymentCancellation(targetDeploymentId).pipe(Effect.provideService(InngestClient, client)))).toBe(true);
      const output = await running;
      expect(output.result).toEqual({ environmentDeploymentId: targetDeploymentId, status: "cancelled", skipped: true });
      expect((await imageBuildRows(targetDeploymentId)).map(({ status }) => status)).toEqual(["cancelled", "cancelled"]);
      expect(fake.prepared).toEqual([]);
    }, 20_000);

    describe("one building attempt, one pending attempt", () => {
      const push = (deliveryId: string) => ({ origin: "github" as const, deliveryId, branchEvaluationRevision: 1, installationId: 17, repositoryId: 42 });
      /** Admits through the real admission path, then dispatches after commit as every caller does. */
      async function admit(triggerOrigin: Parameters<typeof admitEnvironmentDeployment>[0]["triggerOrigin"]) {
        const admitted = await harness.runTransaction(() => admitEnvironmentDeployment({ environmentId, savedStateSnapshotId: savedId, triggerOrigin, message: null }));
        await harness.runEffect(dispatchEnvironmentDeployment({ environmentDeploymentId: admitted.id, environmentId })
          .pipe(Effect.provideService(InngestClient, client)));
        return admitted.id;
      }
      /** The deploying attempt holds the slot while the target's run starts: the target is the building attempt. */
      async function building(fake: Fake) {
        await harness.db.update(schema.environmentDeployment).set({ status: "deploying" })
          .where(eq(schema.environmentDeployment.id, activeDeploymentId));
        await engine(fake, targetDeploymentId, targetRunId).executeStep("record-inngest-run");
        expect(await attempt(targetDeploymentId)).toMatchObject({ status: "queued", inngestRunId: targetRunId });
      }
      async function expectPendingDispatched(pendingId: string) {
        await vi.waitFor(() => expect(dispatched).toEqual([pendingId]));
        expect(await attempt(pendingId)).toMatchObject({ status: "queued", inngestRunId: null, dispatchRequestedAt: expect.any(Date) });
        // The deploying attempt keeps the slot throughout.
        expect(await attempt(activeDeploymentId)).toMatchObject({ status: "deploying", inngestRunId: "active-deployment-run" });
      }

      it("keeps admissions pending behind the building attempt, the newest replacing it, until the building attempt takes the slot", async () => {
        const fake: Fake = { builds: [], failImage: null, hold: false, prepared: [] };
        await building(fake);
        // Its builds finish, but it stays queued waiting for the slot.
        expect((await engine(fake, targetDeploymentId, targetRunId).executeStep("mark-deployment-planning")).result).toEqual({ state: "blocked" });

        // A push is admitted as the pending attempt and not dispatched, even though the building attempt's builds are done.
        const pendingId = await admit(push("second"));
        expect(pendingId).not.toBe(targetDeploymentId);
        expect(await attempt(pendingId)).toMatchObject({ status: "queued", inngestRunId: null, dispatchRequestedAt: null });
        // A third push, then a manual deploy, replace the pending attempt; the building one is untouched.
        expect(await admit(push("third"))).toBe(pendingId);
        expect((await attempt(pendingId))?.triggerOrigin).toMatchObject({ deliveryId: "third" });
        expect(await admit({ origin: "manual", actorId: userId })).toBe(pendingId);
        expect((await attempt(pendingId))?.triggerOrigin).toEqual({ origin: "manual", actorId: userId });
        expect(await attempt(targetDeploymentId)).toMatchObject({ status: "queued", inngestRunId: targetRunId, triggerOrigin: { origin: "manual" } });
        expect(await attempt(activeDeploymentId)).toMatchObject({ status: "deploying" });
        expect(dispatched).toEqual([]);

        // The slot frees; the building attempt takes it, and only then is the pending attempt dispatched.
        await harness.db.update(schema.environmentDeployment).set({ status: "applied", finishedAt: new Date() })
          .where(eq(schema.environmentDeployment.id, activeDeploymentId));
        // The earlier run may still be polling for the slot and take it first; either way the building attempt starts planning.
        await engine(fake, targetDeploymentId, targetRunId).executeStep("mark-deployment-planning");
        await vi.waitFor(() => expect(dispatched).toEqual([pendingId]));
        expect(await attempt(targetDeploymentId)).toMatchObject({ status: "planning", inngestRunId: targetRunId });
        expect(await attempt(pendingId)).toMatchObject({ status: "queued", inngestRunId: null, dispatchRequestedAt: expect.any(Date) });
      }, 20_000);

      it("dispatches the pending attempt when the building attempt fails", async () => {
        const fake: Fake = { builds: [], failImage: "web", hold: false, prepared: [] };
        await building(fake);
        const pendingId = await admit(push("second"));
        expect(dispatched).toEqual([]);
        const failed = await engine(fake, targetDeploymentId, targetRunId).execute();
        expect(failed.error).toEqual(expect.objectContaining({ message: "Image Build failed: web." }));
        expect(await attempt(targetDeploymentId)).toMatchObject({ status: "failed" });
        await expectPendingDispatched(pendingId);
      }, 20_000);

      it("reports Deploy now on the pending attempt as waiting while an attempt builds", async () => {
        await building({ builds: [], failImage: null, hold: false, prepared: [] });
        const pendingId = await admit(push("second"));
        const deployNow = await harness.runEffect(dispatchPendingDeployment(environmentId).pipe(Effect.provideService(InngestClient, client)));
        expect(deployNow).toEqual({ state: "pending" });
        expect(await attempt(pendingId)).toMatchObject({ dispatchRequestedAt: null });
        expect(dispatched).toEqual([]);
      }, 20_000);

      it("sweeps a pending attempt whose dispatch was lost, and leaves one behind a building attempt", async () => {
        await building({ builds: [], failImage: null, hold: false, prepared: [] });
        const pendingId = await admit(push("second"));
        const sweep = () => harness.runEffect(dispatchStrandedPendingDeployments().pipe(Effect.provideService(InngestClient, client)));
        expect(await sweep()).toBe(0);
        expect(dispatched).toEqual([]);
        // The building attempt left queued, but its post-commit dispatch never ran (a crash).
        await harness.db.update(schema.environmentDeployment).set({ status: "failed", finishedAt: new Date() })
          .where(eq(schema.environmentDeployment.id, targetDeploymentId));
        expect(await sweep()).toBe(1);
        await expectPendingDispatched(pendingId);
        // A resend is harmless: the event ID is deterministic per attempt.
        await sweep();
        expect(new Set(dispatched)).toEqual(new Set([pendingId]));
      }, 20_000);

      it("dispatches the pending attempt when the building attempt is cancelled", async () => {
        await building({ builds: [], failImage: null, hold: false, prepared: [] });
        const pendingId = await admit(push("second"));
        expect(dispatched).toEqual([]);
        expect(await harness.runEffect(requestDeploymentCancellation(targetDeploymentId).pipe(Effect.provideService(InngestClient, client)))).toBe(true);
        expect(await attempt(targetDeploymentId)).toMatchObject({ status: "cancelled" });
        await expectPendingDispatched(pendingId);
      }, 20_000);
    });
  });
});
