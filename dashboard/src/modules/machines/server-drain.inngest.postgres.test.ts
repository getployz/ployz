import type { ConfigStore, DrainReport, DrainScope } from "@ployz/sdk";
import { InngestTestEngine, mockCtx } from "@inngest/test";
import { Effect, Fiber } from "effect";
import { TestClock } from "effect/testing";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { InngestClient } from "#/modules/inngest/client";
import { createCancelServerDrain, createCloseStaleServerDrains, createDrainServer } from "#/modules/machines/server-drain.inngest";
import { DRAIN_RUNNING_LIMIT_MS } from "#/modules/machines/server-drain";
import { executeDrainOnce, latestDrainOf, listLatestServerDrains, requestServerDrain } from "#/modules/machines/server-drain.server";
import { applyServerPolicyChangeActivity, requestServerPolicyChange } from "#/modules/machines/server-policy.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { PloyzProviderError, type PloyzSdkError, type PloyzSession } from "#/modules/runtime/ployz.server";
import type { Database } from "#/server/database.server";
import { serverDrainAttempt } from "#/modules/machines/tables";
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
      failure: { stage: "not_serving", from: web2, to: web1, detail: "health check timed out", replacement_removed: true },
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
  /** The cancellation Inngest sends for `runId`, naming the event that started it. */
  const cancel = (runId: string, functionId = "drain-server", attemptId = requestId) => new InngestTestEngine({
    function: createCancelServerDrain(new Inngest({ id: "test" }), runEffect),
    events: [{
      name: "inngest/function.cancelled",
      data: {
        function_id: functionId,
        run_id: runId,
        event: { name: "server/drain.requested", data: { attemptId, organizationId, machineId } },
      },
    }],
  }).execute();
  const latest = async () => (await runEffect(listLatestServerDrains(actor, { organizationSlug: "acme" }))).servers;
  const rows = async () => (await harness.pool.query(
    `select id, machine_id, state, inngest_run_id, report, end_code, refusal_message, started_at, ended_at
     from server_drain_attempt order by requested_at, id`,
  )).rows as Array<{
    id: string; machine_id: string; state: string; inngest_run_id: string | null; report: unknown;
    end_code: string | null; refusal_message: string | null; started_at: Date | null; ended_at: Date | null;
  }>;
  /** Bind the row to `runId` and claim it, as a run that reached the Engine leaves it. */
  const claim = (id: string, runId: string) => harness.pool.query(
    `update server_drain_attempt set inngest_run_id = $2, state = 'running', started_at = now() where id = $1`, [id, runId]);
  const request0 = { attemptId: requestId, organizationId, machineId };
  const sweep = () => new InngestTestEngine({ function: createCloseStaleServerDrains(new Inngest({ id: "test" }), runEffect) }).execute();
  /** Each row's state by the last two characters of its id. */
  const states = (all: Array<{ id: string; state: string }>) => Object.fromEntries(all.map(({ id, state }) => [id.slice(-2), state]));

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
    const event = {
      id: `server-drain-${requestId}`,
      name: "server/drain.requested",
      data: { attemptId: requestId, organizationId, machineId },
    };
    expect(await request()).toEqual({ attemptId: requestId, state: "pending", requestedAt: expect.any(String) });
    expect(send.mock.calls).toEqual([[event]]);

    expect(await request(secondTab)).toMatchObject({ attemptId: requestId, state: "pending" });
    await claim(requestId, "run-1");
    expect(await request(secondTab)).toMatchObject({ attemptId: requestId, state: "running" });
    expect(await request()).toMatchObject({ attemptId: requestId, state: "running" });
    expect(send.mock.calls).toEqual([[event]]);
    expect((await rows()).map(({ id }) => id)).toEqual([requestId]);

    expect(await request(secondTab, otherMachineId)).toMatchObject({ attemptId: secondTab, state: "pending" });
  });

  it("sends the same event again when the same request finds its row unbound, and not once a run bound it", async () => {
    await request();
    expect(await request()).toMatchObject({ attemptId: requestId, state: "pending" });
    expect(send.mock.calls.map(([event]) => event)).toEqual([
      expect.objectContaining({ id: `server-drain-${requestId}` }),
      expect.objectContaining({ id: `server-drain-${requestId}` }),
    ]);

    await harness.pool.query(`update server_drain_attempt set inngest_run_id = 'run-1' where id = $1`, [requestId]);
    expect(await request()).toMatchObject({ attemptId: requestId, state: "pending" });
    expect(send).toHaveBeenCalledTimes(2);
  });

  it("fails the unbound pending row when its event can't be sent, so Drain is offered again", async () => {
    send.mockRejectedValueOnce(new Error("inngest unavailable"));
    await expect(request()).rejects.toThrow();
    expect(await rows()).toMatchObject([{ state: "failed", end_code: "not_started", refusal_message: null, inngest_run_id: null }]);
    expect(await latest()).toEqual({ [machineId]: { attemptId: requestId, state: "failed", endedAt: expect.any(String), endCode: "not_started" } });

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
    expect(await rows()).toMatchObject([{ state: "failed", end_code: "refused", refusal_message: "no server named web-2" }]);
    expect(await latest()).toEqual({ [machineId]: {
      attemptId: requestId, state: "failed", endedAt: expect.any(String), endCode: "refused", refusalMessage: "no server named web-2",
    } });
  });

  it("a retried execute step that finds the row running closes it unknown and never asks the Engine", async () => {
    await request();
    await claim(requestId, "run-1");

    expect(await runEffect(executeDrainOnce(request0, "run-1"))).toEqual({ attemptId: requestId, state: "unknown" });
    expect(drainCalls).toEqual([]);
    expect(await rows()).toMatchObject([{ state: "unknown", end_code: "lost", refusal_message: null, ended_at: expect.any(Date) }]);

    expect(await runEffect(executeDrainOnce(request0, "run-1"))).toEqual({ attemptId: requestId, state: "unknown" });
    expect(drainCalls).toEqual([]);
  });

  it("a retried execute step that finds the row finished returns the stored result", async () => {
    await request();
    await drain();
    const [{ inngest_run_id: runId } = { inngest_run_id: null }] = await rows();
    if (runId === null) return expect.fail("the run binds its row");

    expect(await runEffect(executeDrainOnce(request0, runId)))
      .toEqual({ attemptId: requestId, state: "finished" });
    expect(drainCalls).toHaveLength(1);
    expect((await rows())[0]?.report).toEqual(partialReport);
  });

  it("a cancel while the Engine works ends the row unknown, and the report that arrives after replaces it", async () => {
    await request();
    drainAnswer = () => Effect.promise(async () => {
      const [{ inngest_run_id: runId } = { inngest_run_id: null }] = await rows();
      expect((await cancel(String(runId))).result).toEqual({ closed: 1 });
      expect(await rows()).toMatchObject([{ state: "unknown", end_code: "interrupted" }]);
      return partialReport;
    });

    expect((await drain()).result).toEqual({ attemptId: requestId, state: "finished" });
    expect(await rows()).toMatchObject([{ state: "finished", end_code: null, refusal_message: null, report: partialReport }]);
  });

  it("the cancel handler ends its own run's row once: a running one unknown, a pending one cancelled", async () => {
    await request();
    await claim(requestId, "run-1");

    expect((await cancel("run-1", "roll-out-server-upgrade")).result).toEqual({ skipped: true });
    expect((await cancel("run-1")).result).toEqual({ closed: 1 });
    expect((await cancel("run-1")).result).toEqual({ closed: 0 });
    expect(await rows()).toMatchObject([{ state: "unknown", end_code: "interrupted", ended_at: expect.any(Date) }]);

    await request(secondTab, otherMachineId);
    await harness.pool.query(`update server_drain_attempt set inngest_run_id = 'run-2' where id = $1`, [secondTab]);
    expect((await cancel("run-2", "drain-server", secondTab)).result).toEqual({ closed: 1 });
    expect((await rows())[1]).toMatchObject({ state: "cancelled", end_code: "cancelled" });
  });

  it("the cancel handler ends the pending row its run was cancelled before binding, but not one another run bound", async () => {
    await request();
    expect((await cancel("run-never-bound")).result).toEqual({ closed: 1 });
    expect(await rows()).toMatchObject([{ state: "cancelled", end_code: "cancelled", inngest_run_id: null }]);

    await request(secondTab, otherMachineId);
    await harness.pool.query(`update server_drain_attempt set inngest_run_id = 'run-2' where id = $1`, [secondTab]);
    expect((await cancel("run-other", "drain-server", secondTab)).result).toEqual({ closed: 0 });
    expect((await rows())[1]).toMatchObject({ state: "pending", inngest_run_id: "run-2" });
  });

  it("ends the row unknown inside the step when the Engine gives no answer in a day", async () => {
    let asked: () => void = () => {};
    const engineAsked = new Promise<void>((resolve) => { asked = resolve; });
    drainAnswer = () => Effect.suspend(() => {
      asked();
      return Effect.never;
    });
    await request();
    await harness.pool.query(`update server_drain_attempt set inngest_run_id = 'run-1' where id = $1`, [requestId]);

    const ended = await runEffect(Effect.gen(function* () {
      const fiber = yield* Effect.forkChild(executeDrainOnce(request0, "run-1"));
      yield* Effect.promise(() => engineAsked);
      yield* TestClock.adjust(DRAIN_RUNNING_LIMIT_MS);
      return yield* Fiber.join(fiber);
    }).pipe(Effect.provide(TestClock.layer())));

    expect(ended).toEqual({ attemptId: requestId, state: "unknown" });
    expect(await rows()).toMatchObject([{ state: "unknown", end_code: "lost", refusal_message: null }]);
  });

  it("stores an end only in the state its code ends in, with words exactly for a refusal", async () => {
    const end = (state: string, code: string, message: string | null) => harness.pool.query(
      `insert into server_drain_attempt (id, organization_id, machine_id, state, inngest_run_id, started_at, ended_at, end_code, refusal_message)
       values (gen_random_uuid(), $1, $2, $3, gen_random_uuid()::text, now(), now(), $4, $5)`,
      [organizationId, machineId, state, code, message],
    );
    await expect(end("failed", "lost", null)).rejects.toThrow(/server_drain_attempt_end_code_check/);
    await expect(end("failed", "refused", null)).rejects.toThrow(/server_drain_attempt_refusal_check/);
    await expect(end("failed", "not_started", "no server named web-2")).rejects.toThrow(/server_drain_attempt_refusal_check/);
    await end("unknown", "lost", null);
    await end("failed", "refused", "no server named web-2");
    expect((await rows()).map(({ state, end_code: code }) => [state, code])).toEqual(expect.arrayContaining([
      ["unknown", "lost"],
      ["failed", "refused"],
    ]));
  });

  it("reads a row without its state's evidence as a defect, not as a Drain", async () => {
    await request();
    const [pending] = await harness.db.select().from(serverDrainAttempt);
    if (pending === undefined) return expect.fail("the request writes its row");
    await expect(Effect.runPromise(latestDrainOf({ ...pending, state: "running" })))
      .rejects.toThrow(`Drain ${requestId} is running without its start time.`);
    await expect(Effect.runPromise(latestDrainOf({ ...pending, state: "unknown" })))
      .rejects.toThrow(`Drain ${requestId} is unknown without its end.`);
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

    expect((await rows()).map(({ state, end_code: code }) => [state, code])).toEqual([
      ["failed", "not_started"],
      ["unknown", "lost"],
    ]);
  });

  it("the hourly sweep fails requests no run picked up in time and closes day-old Drains unknown", async () => {
    await harness.pool.query(`
      insert into server_drain_attempt (id, organization_id, machine_id, state, requested_at)
      values ('00000000-0000-4000-8000-0000000000b1', '${organizationId}', '${machineId}', 'pending', now() - interval '20 minutes');
      insert into server_drain_attempt (id, organization_id, machine_id, state, inngest_run_id, requested_at, started_at)
      values ('00000000-0000-4000-8000-0000000000b2', '${organizationId}', '${otherMachineId}', 'running', 'run-old', now() - interval '25 hours', now() - interval '25 hours');
    `);

    // b1 waited behind b2; its time starts once b2 ends.
    expect((await sweep()).result).toEqual({ failed: 0, unknown: 1 });
    await harness.pool.query(`update server_drain_attempt set ended_at = now() - interval '20 minutes' where state = 'unknown'`);
    expect((await sweep()).result).toEqual({ failed: 1, unknown: 0 });
    expect((await sweep()).result).toEqual({ failed: 0, unknown: 0 });
    expect(states(await rows())).toEqual({ b1: "failed", b2: "unknown" });
  });

  it("the hourly sweep leaves a request waiting its turn: behind a running Drain, an earlier request, or one that just ended", async () => {
    const otherOrganization = "00000000-0000-4000-8000-000000000d03";
    const lastOrganization = "00000000-0000-4000-8000-000000000d04";
    await harness.pool.query(`
      insert into organization (id, name, slug) values ('${otherOrganization}', 'Other', 'other'), ('${lastOrganization}', 'Last', 'last');
      insert into server_drain_attempt (id, organization_id, machine_id, state, requested_at)
      values ('00000000-0000-4000-8000-0000000000b4', '${otherOrganization}', '${machineId}', 'pending', now() - interval '20 minutes'),
             ('00000000-0000-4000-8000-0000000000c1', '${organizationId}', '${machineId}', 'pending', now() - interval '40 minutes'),
             ('00000000-0000-4000-8000-0000000000c2', '${organizationId}', '${otherMachineId}', 'pending', now() - interval '30 minutes'),
             ('00000000-0000-4000-8000-0000000000d1', '${lastOrganization}', '${machineId}', 'pending', now() - interval '30 minutes');
      insert into server_drain_attempt (id, organization_id, machine_id, state, inngest_run_id, requested_at, started_at)
      values ('00000000-0000-4000-8000-0000000000b3', '${otherOrganization}', '${otherMachineId}', 'running', 'run-new', now() - interval '1 hour', now() - interval '1 hour');
      insert into server_drain_attempt (id, organization_id, machine_id, state, inngest_run_id, requested_at, started_at, ended_at, end_code)
      values ('00000000-0000-4000-8000-0000000000d2', '${lastOrganization}', '${otherMachineId}', 'unknown', 'run-last', now() - interval '2 hours', now() - interval '2 hours', now() - interval '5 minutes', 'lost');
    `);

    // c1 is first in its Organization and stale; c2 waits behind it, and its time starts once c1 ends.
    expect((await sweep()).result).toEqual({ failed: 1, unknown: 0 });
    expect((await sweep()).result).toEqual({ failed: 0, unknown: 0 });
    expect(states(await rows())).toEqual({ c1: "failed", c2: "pending", b4: "pending", b3: "running", d2: "unknown", d1: "pending" });
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
