import type { IngressCheck, MachineId } from "@ployz/sdk";
import { Effect, Fiber } from "effect";
import { TestClock } from "effect/testing";
import { afterAll, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { WEB_TRAFFIC_RUNNING_LIMIT_MS } from "#/modules/machines/server-web-traffic";
import { executeWebTrafficOnce } from "#/modules/machines/server-web-traffic.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { PloyzProviderError, type PloyzSdkError, type PloyzSession } from "#/modules/runtime/ployz.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const organizationId = "00000000-0000-4000-8000-000000000e01";
const userId = "00000000-0000-4000-8000-000000000e02";
const machineId = "a".repeat(32);
const attemptId = "00000000-0000-4000-8000-0000000000e1";
const request = { attemptId, organizationId, machineId };

describe("executeWebTrafficOnce", () => {
  let harness: PostgresTestHarness;
  /** The role the Server holds, as each `updateMachine` leaves it. */
  let role: boolean;
  let startAnswer: () => Effect.Effect<IngressCheck, PloyzSdkError>;
  let stopAnswer: () => Effect.Effect<void, PloyzSdkError>;

  const execute = () => executeWebTrafficOnce(request, "run-1").pipe(
    Effect.provideService(OrganizationRuntime, {
      cancel: () => Effect.void,
      open: () => Effect.succeed({ status: "connected" as const, connected: asTestDouble<PloyzSession>()({
        updateMachine: (_machine: string, update: { accepts_ingress: boolean }) => Effect.sync(() => { role = update.accepts_ingress; }),
        startIngress: () => startAnswer(),
        stopIngress: () => stopAnswer(),
      }) }),
    }),
  );
  /** A pending row bound to run-1, as the bind step leaves it. */
  const pending = (want: "on" | "off") => harness.pool.query(
    `insert into server_web_traffic_attempt (id, organization_id, machine_id, requested_by_user_id, want, inngest_run_id)
     values ($1, $2, $3, $4, $5, 'run-1')`,
    [attemptId, organizationId, machineId, userId, want],
  );
  const row = async () => (await harness.pool.query(
    `select state, failure from server_web_traffic_attempt where id = $1`, [attemptId])).rows[0] as { state: string; failure: string | null };
  const unreachable = (message: string) => Effect.fail(new PloyzProviderError({ operation: "start ingress", cause: { message } }));

  beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
  afterAll(async () => { await harness?.stop(); });

  beforeEach(async () => {
    startAnswer = () => Effect.succeed({ repairs: [], unchecked: [] });
    stopAnswer = () => Effect.void;
    await harness.pool.query(`
      truncate table organization, "user", operation_outbox cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
      insert into "user" (id, email, name) values ('${userId}', 'member@example.com', 'Member');
    `);
  });

  it("turns the role on and leaves it when the web server starts", async () => {
    role = false;
    await pending("on");
    expect(await harness.runEffect(execute())).toEqual({ attemptId, state: "finished" });
    expect(role).toBe(true);
    expect(await row()).toEqual({ state: "finished", failure: null });
  });

  it("puts the role back when the start throws, and ends the row failed with the Engine's words", async () => {
    role = false;
    startAnswer = () => unreachable("machine role not observed within 30s");
    await pending("on");
    expect(await harness.runEffect(execute())).toEqual({ attemptId, state: "failed" });
    expect(role).toBe(false);
    expect(await row()).toEqual({ state: "failed", failure: "machine role not observed within 30s" });
  });

  it("puts the role back when the start refuses", async () => {
    role = false;
    startAnswer = () => Effect.succeed({
      repairs: [{ machine: machineId as MachineId, server: "web-1", action: "create", failure: { type: "port_taken", cause: "0.0.0.0:80: bind: address already in use" } }],
      unchecked: [],
    });
    await pending("on");
    await harness.runEffect(execute());
    expect(role).toBe(false);
    expect(await row()).toEqual({ state: "failed", failure: "Port 80 is in use on web-1. Free it, then try again." });
  });

  it("puts the role back when the start runs out of time", async () => {
    role = false;
    let asked: () => void = () => {};
    const engineAsked = new Promise<void>((resolve) => { asked = resolve; });
    startAnswer = () => Effect.suspend(() => {
      asked();
      return Effect.never;
    });
    await pending("on");
    const ended = await harness.runEffect(Effect.gen(function* () {
      const fiber = yield* Effect.forkChild(execute());
      yield* Effect.promise(() => engineAsked);
      yield* TestClock.adjust(WEB_TRAFFIC_RUNNING_LIMIT_MS);
      return yield* Fiber.join(fiber);
    }).pipe(Effect.provide(TestClock.layer())));
    expect(ended).toEqual({ attemptId, state: "failed" });
    expect(role).toBe(false);
    expect(await row()).toEqual({ state: "failed", failure: "Your servers didn't answer in time. Try again." });
  });

  it("keeps the role off when the stop throws, since the web server may already be gone", async () => {
    role = true;
    stopAnswer = () => unreachable("connection reset");
    await pending("off");
    await harness.runEffect(execute());
    expect(role).toBe(false);
    expect(await row()).toEqual({ state: "failed", failure: "connection reset" });
  });
});
