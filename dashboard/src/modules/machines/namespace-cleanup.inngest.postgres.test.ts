import type { ConfigStore, DataLossConfirmation, DeployOutcome, ExecutionError, MachineId } from "@ployz/sdk";
import { InngestTestEngine, mockCtx } from "@inngest/test";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { InngestClient } from "#/modules/inngest/client";
import { createCancelNamespaceCleanup, createCleanNamespace } from "#/modules/machines/namespace-cleanup.inngest";
import {
  executeCleanupOnce, readCliNamespaceCleanup, requestNamespaceCleanup,
} from "#/modules/machines/namespace-cleanup.server";
import { MissingDataLossIdentities } from "#/modules/runtime/data-loss-confirm";
import type { DataLossIdentity } from "#/modules/runtime/data-loss-identity";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { PloyzProviderError, type PloyzSdkError, type PloyzSession } from "#/modules/runtime/ployz.server";
import type { Database } from "#/server/database.server";
import { makeInngestEffectRunner, type runInngestEffect } from "#/server/run.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const organizationId = "00000000-0000-4000-8000-000000000c01";
const userId = "00000000-0000-4000-8000-000000000c02";
const caller = { organizationId, userId };
const data: DataLossIdentity = { kind: "docker_volume", id: { machine_id: "a".repeat(32) as MachineId, name: "left-behind_data" } };
const cache: DataLossIdentity = { kind: "docker_volume", id: { machine_id: "b".repeat(32) as MachineId, name: "left-behind_cache" } };

const inngest = new Inngest({ id: "namespace-cleanup-test" });
const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

describe("clean-namespace", () => {
  let harness: PostgresTestHarness;
  let destroyCalls: Array<[string, DataLossConfirmation]>;
  let destroyAnswer: () => Effect.Effect<DeployOutcome<ExecutionError>, PloyzSdkError>;
  let connected: boolean;
  let owned: string[];

  const runEffect = makeInngestEffectRunner(<A, E>(operation: Effect.Effect<A, E, Database | OrganizationRuntime | InngestClient | CloudStore>) =>
    harness.runEffect(operation.pipe(
      Effect.provideService(InngestClient, inngest),
      Effect.provideService(CloudStore, {
        open: Effect.succeed(asTestDouble<ConfigStore>()({
          read: async () => ({ namespaces: owned.map((namespace) => ({ namespace })) }),
        })),
      }),
      Effect.provideService(OrganizationRuntime, {
        cancel: () => Effect.void,
        open: () => Effect.succeed(connected
          ? { status: "connected" as const, connected: asTestDouble<PloyzSession>()({
            destroyNamespace: (namespace: string, confirmation: DataLossConfirmation) => {
              destroyCalls.push([namespace, confirmation]);
              return destroyAnswer();
            },
          }) }
          : { status: "no_connection" as const }),
      }),
    ))) as typeof runInngestEffect;

  const request = (approvalId: string | null = null) =>
    runEffect(requestNamespaceCleanup(caller, { namespace: "left-behind", confirmDataLoss: [data, cache], approvalId }));
  const clean = (cleanupId: string, runId = "run-clean") => new InngestTestEngine({
    function: createCleanNamespace(new Inngest({ id: "test" }), runEffect),
    events: [{ name: "namespace/cleanup.requested", data: { cleanupId, organizationId } }],
    transformCtx: (ctx) => ({ ...mockCtx(ctx), runId }),
  }).execute();
  const cancel = (runId: string, cleanupId: string) => new InngestTestEngine({
    function: createCancelNamespaceCleanup(new Inngest({ id: "test" }), runEffect),
    events: [{
      name: "inngest/function.cancelled",
      data: {
        function_id: "test-clean-namespace",
        run_id: runId,
        event: { name: "namespace/cleanup.requested", data: { cleanupId, organizationId } },
      },
    }],
  }).execute();
  const read = (cleanupId: string) => runEffect(readCliNamespaceCleanup(organizationId, cleanupId));
  const rows = async () => (await harness.pool.query(
    `select id, state, inngest_run_id, end_code, volumes from namespace_cleanup order by requested_at, id`,
  )).rows as Array<{ id: string; state: string; inngest_run_id: string | null; end_code: string | null; volumes: string[] | null }>;
  const claim = (id: string, runId: string) => harness.pool.query(
    `update namespace_cleanup set inngest_run_id = $2, state = 'running', started_at = now() where id = $1`, [id, runId]);

  beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
  afterAll(async () => { await harness?.stop(); });

  beforeEach(async () => {
    destroyCalls = [];
    destroyAnswer = () => Effect.succeed({ type: "success", completed: [] });
    connected = true;
    owned = [];
    send.mockReset();
    send.mockResolvedValue({ ids: [] });
    await harness.pool.query(`
      truncate table organization, "user" cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
      insert into "user" (id, email, name) values ('${userId}', 'member@example.com', 'Member');
    `);
  });

  it("deletes exactly the confirmed Volumes once, and a second run of the same event never asks again", async () => {
    const cleanupId = await request();
    expect(send.mock.calls).toEqual([[expect.objectContaining({ id: `namespace-cleanup-${cleanupId}` })]]);
    expect(await read(cleanupId)).toEqual({ state: "pending" });

    expect((await clean(cleanupId)).result).toEqual({ cleanupId, state: "finished" });
    expect(destroyCalls).toEqual([["left-behind", { confirmed: [data, cache] }]]);
    expect(await read(cleanupId)).toEqual({ state: "finished", volumes: ["left-behind_cache", "left-behind_data"] });

    expect((await clean(cleanupId, "run-duplicate")).result).toEqual({ cleanupId, state: "finished" });
    expect(destroyCalls).toHaveLength(1);
  });

  it("answers a retry carrying the consumed approval with the same clean", async () => {
    const approvalId = "00000000-0000-4000-8000-000000000ca1";
    const cleanupId = await request(approvalId);
    await clean(cleanupId);

    expect(await request(approvalId)).toBe(cleanupId);
    expect((await rows()).map(({ id }) => id)).toEqual([cleanupId]);
  });

  it("leaves the row pending while the Cluster is unreachable, so the retried step cleans it", async () => {
    const cleanupId = await request();
    await harness.pool.query(`update namespace_cleanup set inngest_run_id = 'run-1' where id = $1`, [cleanupId]);
    connected = false;

    await expect(runEffect(executeCleanupOnce({ cleanupId, organizationId }, "run-1"))).rejects.toThrow();
    expect(await rows()).toMatchObject([{ state: "pending" }]);

    connected = true;
    expect(await runEffect(executeCleanupOnce({ cleanupId, organizationId }, "run-1"))).toEqual({ cleanupId, state: "finished" });
    expect(destroyCalls).toHaveLength(1);
  });

  it("refuses a clean whose Namespace an Environment took while it waited, and never asks the Engine", async () => {
    const cleanupId = await request();
    await harness.pool.query(`update namespace_cleanup set inngest_run_id = 'run-1' where id = $1`, [cleanupId]);
    connected = false;
    await expect(runEffect(executeCleanupOnce({ cleanupId, organizationId }, "run-1"))).rejects.toThrow();

    owned = ["left-behind"];
    connected = true;
    expect(await runEffect(executeCleanupOnce({ cleanupId, organizationId }, "run-1"))).toEqual({ cleanupId, state: "failed" });
    expect(destroyCalls).toEqual([]);
    expect(await read(cleanupId)).toEqual({
      state: "ended", code: "refused", message: "left-behind belongs to an Environment now. Nothing was removed.",
    });
  });

  it("refuses a queued clean whose Namespace became owned before its run", async () => {
    const cleanupId = await request();
    owned = ["left-behind"];

    expect((await clean(cleanupId)).result).toEqual({ cleanupId, state: "failed" });
    expect(destroyCalls).toEqual([]);
  });

  it("a retried step that finds the row running ends it unknown, and never asks the Engine", async () => {
    const cleanupId = await request();
    await claim(cleanupId, "run-1");

    expect(await runEffect(executeCleanupOnce({ cleanupId, organizationId }, "run-1"))).toEqual({ cleanupId, state: "unknown" });
    expect(await runEffect(executeCleanupOnce({ cleanupId, organizationId }, "run-1"))).toEqual({ cleanupId, state: "unknown" });
    expect(destroyCalls).toEqual([]);
    expect(await read(cleanupId)).toEqual({
      state: "ended", code: "lost", message: "Cloud lost track of the clean; some Volumes may be gone.",
    });
  });

  it("ends unknown when the Engine errors mid-clean, and a later run never asks again", async () => {
    destroyAnswer = () => Effect.fail(new PloyzProviderError({ operation: "remove namespace", cause: new Error("connection reset") }));
    const cleanupId = await request();

    expect((await clean(cleanupId)).result).toEqual({ cleanupId, state: "unknown" });
    expect((await clean(cleanupId, "run-again")).result).toEqual({ cleanupId, state: "unknown" });
    expect(destroyCalls).toHaveLength(1);
    expect(await rows()).toMatchObject([{ state: "unknown", end_code: "lost", volumes: null }]);
  });

  it("ends failed with words when the Servers hold Volumes the clean didn't confirm", async () => {
    destroyAnswer = () => Effect.fail(new MissingDataLossIdentities([data]));
    const cleanupId = await request();

    expect((await clean(cleanupId)).result).toEqual({ cleanupId, state: "failed" });
    expect(await read(cleanupId)).toEqual({
      state: "ended", code: "refused", message: "The servers hold Volumes this clean didn't confirm. Nothing was removed.",
    });
  });

  it("the cancel handler ends a pending row cancelled and a running one unknown", async () => {
    const pending = await request();
    expect((await cancel("run-never-bound", pending)).result).toEqual({ closed: 1 });
    expect(await read(pending)).toMatchObject({ state: "ended", code: "cancelled" });

    const running = await request();
    await claim(running, "run-2");
    expect((await cancel("run-2", running)).result).toEqual({ closed: 1 });
    expect((await cancel("run-2", running)).result).toEqual({ closed: 0 });
    expect(await read(running)).toMatchObject({ state: "ended", code: "interrupted" });
    expect(destroyCalls).toEqual([]);
  });
});
