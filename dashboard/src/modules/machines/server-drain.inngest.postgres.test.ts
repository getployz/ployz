import type { ConfigStore, DrainReport, DrainScope } from "@ployz/sdk";
import { InngestTestEngine, mockCtx } from "@inngest/test";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { InngestClient } from "#/modules/inngest/client";
import { createCancelServerDrain, createCloseStaleServerDrains, createDrainServer } from "#/modules/machines/server-drain.inngest";
import { executeDrainOnce, listLatestServerDrains, requestServerDrain } from "#/modules/machines/server-drain.server";
import { applyServerPolicyChangeActivity, requestServerPolicyChange } from "#/modules/machines/server-policy.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { PloyzProviderError, type PloyzSdkError, type PloyzSession } from "#/modules/runtime/ployz.server";
import type { Database } from "#/server/database.server";
import { makeInngestEffectRunner, type runInngestEffect } from "#/server/run.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const organizationId = "00000000-0000-4000-8000-000000000d01";
const userId = "00000000-0000-4000-8000-000000000d02";
const actor = { userId };
const machineId = "a".repeat(32);
const otherMachineId = "b".repeat(32);
const requestId = "00000000-0000-4000-8000-0000000000a1";
const secondTab = "00000000-0000-4000-8000-0000000000a2";
const web1 = { id: "1".repeat(32), name: "web-1" };
const web2 = { id: machineId, name: "web-2" };

const inngest = new Inngest({ id: "server-drain-test" });
const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

/** A partial report: one moved, one failed after a move, one stayed for its volume, one Global stopped, then a stop. */
const partialReport = {
  server: asTestDouble<DrainReport["server"]>()({ id: machineId, name: "web-2" }),
  services_role: "turned_off",
  services: [
    { service: "shop/metrics", result: "retired" },
    { service: "shop/api", result: "moved", moves: [{ from: web2, to: web1 }] },
    {
      service: "shop/worker",
      result: "failed",
      moves: [{ from: web2, to: web1 }],
      failure: { stage: "not_serving", from: web2, to: web1, detail: "health check timed out" },
    },
    { service: "shop/db", result: "stays", reason: { kind: "volume", volume: "pg-data", server: web2 } },
    { service: "shop/web", result: "not_attempted" },
  ],
  stopped: { kind: "cancelled" },
  remaining: { kind: "observed", services: ["shop/db", "shop/worker", "shop/web"] },
} as DrainReport;

describe("drain-server", () => {
  let harness: PostgresTestHarness;
  /** The Namespaces the Config Store says the Organization's Environments own. */
  let namespaces: string[];
  let drainCalls: Array<[string, DrainScope]>;
  /** What the Engine answers a Drain with. */
  let drainAnswer: () => Effect.Effect<DrainReport, PloyzSdkError>;
  let updates: unknown[][];

  const runEffect = makeInngestEffectRunner(<A, E>(operation: Effect.Effect<A, E, Database | OrganizationRuntime | InngestClient | CloudStore>) =>
    harness.runEffect(operation.pipe(
      Effect.provideService(InngestClient, inngest),
      Effect.provideService(CloudStore, {
        open: Effect.succeed(asTestDouble<ConfigStore>()({
          read: async () => ({ namespaces: namespaces.map((namespace) => ({ namespace })) }),
        })),
      }),
      Effect.provideService(OrganizationRuntime, {
        cancel: () => Effect.void,
        open: () => Effect.succeed({ status: "connected" as const, connected: asTestDouble<PloyzSession>()({
          drainMachine: (machine: string, scope: DrainScope) => {
            drainCalls.push([machine, scope]);
            return drainAnswer();
          },
          updateMachine: (...args: unknown[]) => Effect.sync(() => void updates.push(args)),
        }) }),
      }),
    ))) as typeof runInngestEffect;

  const request = (id = requestId, machine = machineId) =>
    runEffect(requestServerDrain(actor, { organizationSlug: "acme", machineId: machine, requestId: id }));
  /** One run of the Drain. The test engine mints a run ID per step execution; Inngest keeps one per run. */
  const drain = (attemptId = requestId, machine = machineId, runId = "run-drain") => new InngestTestEngine({
    function: createDrainServer(new Inngest({ id: "test" }), runEffect),
    events: [{ name: "server/drain.requested", data: { attemptId, organizationId, machineId: machine } }],
    transformCtx: (ctx) => ({ ...mockCtx(ctx), runId }),
  }).execute();
  const cancel = (runId: string, functionId = "drain-server") => new InngestTestEngine({
    function: createCancelServerDrain(new Inngest({ id: "test" }), runEffect),
    events: [{ name: "inngest/function.cancelled", data: { function_id: functionId, run_id: runId } }],
  }).execute();
  const latest = async () => (await runEffect(listLatestServerDrains(actor, { organizationSlug: "acme" }))).servers;
  const rows = async () => (await harness.pool.query(
    `select id, machine_id, state, inngest_run_id, report, failure_code, failure_message, started_at, ended_at
     from server_drain_attempt order by requested_at, id`,
  )).rows as Array<{
    id: string; machine_id: string; state: string; inngest_run_id: string | null; report: unknown;
    failure_code: string | null; failure_message: string | null; started_at: Date | null; ended_at: Date | null;
  }>;
  /** Bind the row to `runId` and claim it, as a run that reached the Engine leaves it. */
  const claim = (id: string, runId: string) => harness.pool.query(
    `update server_drain_attempt set inngest_run_id = $2, state = 'running', started_at = now() where id = $1`, [id, runId]);
  const request0 = { attemptId: requestId, organizationId, machineId };

  beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
  afterAll(async () => { await harness?.stop(); });

  beforeEach(async () => {
    namespaces = ["shop-production"];
    drainCalls = [];
    drainAnswer = () => Effect.succeed(partialReport);
    updates = [];
    send.mockReset();
    send.mockResolvedValue({ ids: [] });
    await harness.pool.query(`
      truncate table organization, "user" cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
      insert into "user" (id, email, name) values ('${userId}', 'member@example.com', 'Member');
      insert into member (id, organization_id, user_id, role, created_at)
      values (gen_random_uuid(), '${organizationId}', '${userId}', 'member', now());
    `);
  });

  it("writes one pending row per request: the same UUID returns it, and a second tab gets the active one", async () => {
    expect(await request()).toEqual({ attemptId: requestId, state: "pending", requestedAt: expect.any(String) });
    expect(send.mock.calls).toEqual([[{
      id: `server-drain-${requestId}`,
      name: "server/drain.requested",
      data: { attemptId: requestId, organizationId, machineId },
    }]]);

    expect(await request()).toMatchObject({ attemptId: requestId, state: "pending" });
    expect(await request(secondTab)).toMatchObject({ attemptId: requestId, state: "pending" });
    await claim(requestId, "run-1");
    expect(await request(secondTab)).toMatchObject({ attemptId: requestId, state: "running" });
    expect(send).toHaveBeenCalledTimes(1);
    expect((await rows()).map(({ id }) => id)).toEqual([requestId]);

    expect(await request(secondTab, otherMachineId)).toMatchObject({ attemptId: secondTab, state: "pending" });
  });

  it("fails the unbound pending row when its event can't be sent, so Drain is offered again", async () => {
    send.mockRejectedValueOnce(new Error("inngest unavailable"));
    await expect(request()).rejects.toThrow();
    expect(await rows()).toMatchObject([{ state: "failed", failure_code: "dispatch_failed", inngest_run_id: null }]);
    expect(await latest()).toMatchObject({ [machineId]: { state: "failed", failureCode: "dispatch_failed" } });

    expect(await request(secondTab)).toMatchObject({ attemptId: secondTab, state: "pending" });
  });

  it("asks the Engine once for the owned Namespaces and stores its partial report as the result", async () => {
    await request();
    const output = await drain();

    expect(output.error).toBeUndefined();
    expect(output.result).toEqual({ attemptId: requestId, state: "finished" });
    expect(drainCalls).toEqual([[machineId, { scope: "owned", namespaces: ["shop-production"] }]]);
    expect(await latest()).toEqual({
      [machineId]: { attemptId: requestId, state: "finished", endedAt: expect.any(String), report: partialReport },
    });

    expect((await drain(requestId, machineId, "run-duplicate")).result).toEqual({ attemptId: requestId, state: "finished" });
    expect(drainCalls).toHaveLength(1);
  });

  it("passes an empty Namespace list as owned, so the Engine selects nothing rather than everything", async () => {
    namespaces = [];
    drainAnswer = () => Effect.succeed({ ...partialReport, services: [], stopped: null });
    await request();
    await drain();

    expect(drainCalls).toEqual([[machineId, { scope: "owned", namespaces: [] }]]);
    expect(await rows()).toMatchObject([{ state: "finished" }]);
  });

  it("ends failed with the Engine's words when it refuses, and never asks again", async () => {
    drainAnswer = () => Effect.fail(new PloyzProviderError({
      operation: "drain machine", cause: { code: "not_found", message: "no server named web-2" },
    }));
    await request();
    const output = await drain();

    expect(output.result).toEqual({ attemptId: requestId, state: "failed" });
    expect(drainCalls).toHaveLength(1);
    expect(await rows()).toMatchObject([{ state: "failed", failure_code: "refused", failure_message: "no server named web-2" }]);
    expect(await latest()).toMatchObject({ [machineId]: { state: "failed", failureCode: "refused", failureMessage: "no server named web-2" } });
  });

  it("a retried execute step that finds the row running closes it unknown and never asks the Engine", async () => {
    await request();
    await claim(requestId, "run-1");
    const scope = { scope: "owned", namespaces: ["shop-production"] } satisfies DrainScope;

    expect(await runEffect(executeDrainOnce(request0, "run-1", scope))).toEqual({ attemptId: requestId, state: "unknown" });
    expect(drainCalls).toEqual([]);
    expect(await rows()).toMatchObject([{ state: "unknown", failure_code: "lost", ended_at: expect.any(Date) }]);

    expect(await runEffect(executeDrainOnce(request0, "run-1", scope))).toEqual({ attemptId: requestId, state: "unknown" });
    expect(drainCalls).toEqual([]);
  });

  it("a retried execute step that finds the row finished returns the stored result", async () => {
    await request();
    await drain();
    const [{ inngest_run_id: runId } = { inngest_run_id: null }] = await rows();
    if (runId === null) return expect.fail("the run binds its row");

    expect(await runEffect(executeDrainOnce(request0, runId, { scope: "owned", namespaces: [] })))
      .toEqual({ attemptId: requestId, state: "finished" });
    expect(drainCalls).toHaveLength(1);
    expect((await rows())[0]?.report).toEqual(partialReport);
  });

  it("a finish that arrives after the run was cancelled keeps the row cancelled", async () => {
    await request();
    drainAnswer = () => Effect.promise(async () => {
      const [{ inngest_run_id: runId } = { inngest_run_id: null }] = await rows();
      expect((await cancel(String(runId))).result).toEqual({ closed: 1 });
      return partialReport;
    });

    expect((await drain()).result).toEqual({ attemptId: requestId, state: "cancelled" });
    expect(await rows()).toMatchObject([{ state: "cancelled", failure_code: "cancelled", report: null }]);
  });

  it("the cancel handler closes only its own run's active row, once", async () => {
    await request();
    await claim(requestId, "run-1");

    expect((await cancel("run-1", "roll-out-server-upgrade")).result).toEqual({ skipped: true });
    expect((await cancel("run-1")).result).toEqual({ closed: 1 });
    expect((await cancel("run-1")).result).toEqual({ closed: 0 });
    expect(await rows()).toMatchObject([{ state: "cancelled", failure_code: "cancelled", ended_at: expect.any(Date) }]);
  });

  it("onFailure ends a pending row failed and a running one unknown, and leaves an ended row alone", async () => {
    const onFailure = createDrainServer(new Inngest({ id: "test" }), runEffect).opts.onFailure;
    if (onFailure === undefined) return expect.fail("a failed run closes its row");
    await request();
    await harness.pool.query(`update server_drain_attempt set inngest_run_id = 'run-1' where id = $1`, [requestId]);
    await request(secondTab, otherMachineId);
    await claim(secondTab, "run-2");

    // SAFETY: the failure handler reads only the failed run's ID.
    const fail = (runId: string) => Promise.resolve(onFailure({ event: { data: { run_id: runId } } } as never));
    await fail("run-1");
    await fail("run-2");
    await fail("run-2");

    expect((await rows()).map(({ state, failure_code: code }) => [state, code])).toEqual([
      ["failed", "workflow_failed"],
      ["unknown", "lost"],
    ]);
  });

  it("the hourly sweep fails requests no run picked up and closes day-old Drains unknown, but waits behind a running one", async () => {
    const third = "c".repeat(32);
    const otherOrganization = "00000000-0000-4000-8000-000000000d03";
    await harness.pool.query(`
      insert into organization (id, name, slug) values ('${otherOrganization}', 'Other', 'other');
      insert into server_drain_attempt (id, organization_id, machine_id, state, requested_at)
      values ('00000000-0000-4000-8000-0000000000b1', '${organizationId}', '${machineId}', 'pending', now() - interval '20 minutes'),
             ('00000000-0000-4000-8000-0000000000b4', '${otherOrganization}', '${machineId}', 'pending', now() - interval '20 minutes'),
             ('00000000-0000-4000-8000-0000000000b5', '${otherOrganization}', '${third}', 'pending', now() - interval '5 minutes');
      insert into server_drain_attempt (id, organization_id, machine_id, state, inngest_run_id, requested_at, started_at)
      values ('00000000-0000-4000-8000-0000000000b2', '${organizationId}', '${otherMachineId}', 'running', 'run-old', now() - interval '25 hours', now() - interval '25 hours'),
             ('00000000-0000-4000-8000-0000000000b3', '${otherOrganization}', '${otherMachineId}', 'running', 'run-new', now() - interval '1 hour', now() - interval '1 hour');
    `);
    const sweep = () => new InngestTestEngine({ function: createCloseStaleServerDrains(new Inngest({ id: "test" }), runEffect) }).execute();

    expect((await sweep()).result).toEqual({ failed: 1, unknown: 1 });
    expect((await sweep()).result).toEqual({ failed: 0, unknown: 0 });
    expect(Object.fromEntries((await rows()).map(({ id, state }) => [id.slice(-2), state]))).toEqual({
      b1: "failed",
      b2: "unknown",
      // Queued behind the Other Organization's running Drain: its turn hasn't come.
      b4: "pending",
      b5: "pending",
      b3: "running",
    });
  });

  it("reads each Server's latest Drain", async () => {
    await request();
    await drain();
    await request(secondTab);

    expect(await latest()).toEqual({
      [machineId]: { attemptId: secondTab, state: "pending", requestedAt: expect.any(String) },
    });
  });

  it("Server Policy refuses turning services back on while a Drain is active, at request and at apply", async () => {
    await request();
    send.mockClear();
    const change = (acceptsServices: boolean) => ({ organizationSlug: "acme", machineId, change: { acceptsServices } });

    // The Inngest runner marks a Conflict non-retriable, so a refused apply never retries.
    const conflict = { cause: { _tag: "Conflict", message: "A drain is running on this server. Wait for it to finish." } };
    await expect(runEffect(requestServerPolicyChange(actor, change(true)))).rejects.toMatchObject(conflict);
    await expect(runEffect(Effect.scoped(applyServerPolicyChangeActivity({ organizationId, machineId, change: { acceptsServices: true } }))))
      .rejects.toMatchObject(conflict);
    expect(send).not.toHaveBeenCalled();
    expect(updates).toEqual([]);

    await runEffect(requestServerPolicyChange(actor, change(false)));
    expect(send).toHaveBeenCalledTimes(1);
    await drain();
    await runEffect(Effect.scoped(applyServerPolicyChangeActivity({ organizationId, machineId, change: { acceptsServices: true } })));
    expect(updates).toEqual([[machineId, { accepts_services: true }]]);
  });
});
