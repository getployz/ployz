import type { MachineUpgradeAttempt, MachineUpgradeAttemptId, RuntimeWatchView } from "@ployz/sdk";
import { InngestTestEngine } from "@inngest/test";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { PostHog, type PostHogService } from "#/modules/analytics/posthog.server";
import { InngestClient } from "#/modules/inngest/client";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { PloyzProviderError, type PloyzSession } from "#/modules/runtime/ployz.server";
import {
  runtimeWatchFrameFixture,
  runtimeWatchMachineFixture,
  runtimeWatchMachineObservationFixture,
} from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { createCancelServerUpgrade, createRollOutServerUpgrade } from "#/modules/server-upgrade/server-upgrade.inngest";
import { listLatestServerUpgrades, requestServerUpgrade } from "#/modules/server-upgrade/server-upgrade.server";
import type { Database } from "#/server/database.server";
import { makeInngestEffectRunner, type runInngestEffect } from "#/server/run.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const organizationId = "00000000-0000-4000-8000-000000000c01";
const userId = "00000000-0000-4000-8000-000000000c02";
const machineId = "a".repeat(32);
const inngest = new Inngest({ id: "server-upgrade-test" });
const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

type Attempt = MachineUpgradeAttempt;
const running = (stage: string): Partial<Attempt> => ({ outcome: "running", stage });

describe("roll-out-server-upgrade", () => {
  let harness: PostgresTestHarness;
  let frame: RuntimeWatchView;
  /** What the fake Server answers the request with; `busy` refuses it as Busy. */
  let requestAnswer: Partial<Attempt> | "busy";
  /** What each inspect answers, in order; the last repeats. `unreadable` fails like a restarting daemon. */
  let inspectAnswers: Array<Partial<Attempt> | "unreadable">;
  let requests: Array<{ machine: string; attemptId: string; release: string }>;
  let captured: Parameters<PostHogService["capture"]>[0][];

  const attempt = (attemptId: string, answer: Partial<Attempt>) =>
    ({ attempt_id: attemptId as MachineUpgradeAttemptId, target: "0.2.2", ...answer }) as Attempt;
  const runEffect = makeInngestEffectRunner(<A, E>(operation: Effect.Effect<A, E, Database | OrganizationRuntime | InngestClient>) =>
    harness.runEffect(operation.pipe(
      Effect.provideService(InngestClient, inngest),
      Effect.provideService(PostHog, {
        capture: (input) => Effect.sync(() => void captured.push(input)),
        identify: () => Effect.void,
        identifyOrganization: () => Effect.void,
      }),
      Effect.provideService(OrganizationRuntime, {
        cancel: () => Effect.void,
        open: () => Effect.succeed({ status: "connected" as const, connected: asTestDouble<PloyzSession>()({
          watchFirstFrame: () => Effect.succeed(frame),
          requestMachineUpgrade: (machine: string, attemptId: string, release: string) => {
            requests.push({ machine, attemptId, release });
            return requestAnswer === "busy"
              ? Effect.fail(new PloyzProviderError({ operation: "request machine upgrade", cause: { code: "conflict", message: "busy" } }))
              : Effect.succeed(attempt(attemptId, requestAnswer));
          },
          inspectMachineUpgrade: (_machine: string, attemptId: string) => {
            const answer = inspectAnswers.length > 1 ? inspectAnswers.shift() : inspectAnswers[0];
            return answer === undefined || answer === "unreadable"
              ? Effect.fail(new PloyzProviderError({ operation: "inspect machine upgrade", cause: { code: "unavailable" } }))
              : Effect.succeed(attempt(attemptId, answer));
          },
        }) }),
      }),
    ))) as typeof runInngestEffect;

  const rollOut = () => new InngestTestEngine({
    function: createRollOutServerUpgrade(new Inngest({ id: "test" }), runEffect),
    events: [{ name: "server/upgrade.requested", data: { organizationId, machineId, trigger: "manual", userId } }],
    // Each poll's sleep ends at once; eighty polls are the twenty minutes.
    steps: Array.from({ length: 80 }, (_, poll) => ({ id: `wait-${poll}`, handler: () => undefined })),
  }).execute();
  const rows = async () => (await harness.pool.query(
    `select attempt_id, trigger, requested_by_user_id, channel, from_version, target_version, outcome, stage, error, ended_at
     from server_upgrade_attempt`,
  )).rows as Array<{
    attempt_id: string; trigger: string; requested_by_user_id: string | null; channel: string; from_version: string;
    target_version: string | null; outcome: string; stage: string | null; error: string | null; ended_at: Date | null;
  }>;

  beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
  afterAll(async () => { await harness?.stop(); });

  beforeEach(async () => {
    frame = runtimeWatchFrameFixture({ machines: [runtimeWatchMachineObservationFixture({
      machine: runtimeWatchMachineFixture(machineId, "web-1", {
        runtime: { ...runtimeWatchMachineFixture(machineId, "web-1").runtime, daemon_version: "0.2.1" },
      }),
    })] });
    requestAnswer = { outcome: "accepted" };
    inspectAnswers = [running("restarting"), { outcome: "succeeded", version: "0.2.2" }];
    requests = [];
    captured = [];
    send.mockClear();
    await harness.pool.query(`
      truncate table organization, "user" cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
      insert into "user" (id, email, name) values ('${userId}', 'member@example.com', 'Member');
      insert into member (id, organization_id, user_id, role, created_at)
      values (gen_random_uuid(), '${organizationId}', '${userId}', 'member', now());
    `);
  });

  it("records a manual attempt, requests it along stable, and polls it to success", async () => {
    const output = await rollOut();

    expect(output.error).toBeUndefined();
    expect(output.result).toMatchObject({ outcome: "succeeded" });
    expect(requests).toEqual([{ machine: machineId, attemptId: expect.stringMatching(/^[0-9a-f]{32}$/u), release: "stable" }]);
    expect(await rows()).toEqual([{
      attempt_id: requests[0]?.attemptId,
      trigger: "manual",
      requested_by_user_id: userId,
      channel: "stable",
      from_version: "0.2.1",
      target_version: "0.2.2",
      outcome: "succeeded",
      stage: null,
      error: null,
      ended_at: expect.any(Date),
    }]);
    expect(captured).toEqual([{
      userId,
      organizationId,
      event: "server_upgrade_succeeded",
      properties: { trigger: "manual", channel: "stable", from_version: "0.2.1", to_version: "0.2.2", total_seconds: expect.any(Number) },
    }]);
  });

  it("records a failure with its stage and exact error, polling through an unreadable daemon", async () => {
    const error = "readiness timed out; restored 0.2.1";
    inspectAnswers = [running("activating"), "unreadable", { outcome: "failed", stage: "readiness", error }];

    const output = await rollOut();

    expect(output.result).toMatchObject({ outcome: "failed" });
    expect(await rows()).toMatchObject([{ outcome: "failed", stage: "readiness", error }]);
    expect(captured).toEqual([expect.objectContaining({
      event: "server_upgrade_failed",
      properties: expect.objectContaining({ stage: "readiness", error, from_version: "0.2.1", to_version: "0.2.2" }),
    })]);
  });

  it("records an interrupted attempt with the stage it reached", async () => {
    inspectAnswers = [{ outcome: "interrupted", stage: "restarting" }];

    await rollOut();

    expect(await rows()).toMatchObject([{ outcome: "interrupted", stage: "restarting", error: null }]);
    expect(captured).toHaveLength(1);
    expect(captured[0]).toMatchObject({ event: "server_upgrade_interrupted", properties: { stage: "restarting" } });
    expect(captured[0]?.properties).not.toHaveProperty("error");
  });

  it("records unknown with the last stage seen after twenty minutes without an outcome", async () => {
    inspectAnswers = [running("readiness"), "unreadable"];

    const output = await rollOut();

    expect(output.result).toMatchObject({ outcome: "unknown" });
    expect(await rows()).toMatchObject([{ outcome: "unknown", stage: "readiness", error: null }]);
    expect(captured).toEqual([expect.objectContaining({ event: "server_upgrade_unknown", properties: expect.objectContaining({ stage: "readiness" }) })]);
  });

  it("records nothing and sends nothing when the Server refuses as Busy", async () => {
    requestAnswer = "busy";

    const output = await rollOut();

    expect(output.result).toEqual({ skipped: "busy" });
    expect(await rows()).toEqual([]);
    expect(captured).toEqual([]);
  });

  it("requests nothing for a Server that is building", async () => {
    const observed = frame.machines[0];
    if (observed) observed.machine.runtime.running_builds = 1;

    expect((await rollOut()).result).toEqual({ skipped: "not-online" });
    expect(requests).toEqual([]);
    expect(await rows()).toEqual([]);
  });

  it("a cancelled run closes its attempt as unknown, once", async () => {
    await harness.pool.query(`
      insert into server_upgrade_attempt (organization_id, machine_id, attempt_id, trigger, channel, from_version, stage, inngest_run_id, started_at)
      values ('${organizationId}', '${machineId}', '${"b".repeat(32)}', 'manual', 'stable', '0.2.1', 'restarting', 'run-1', now());
    `);
    const cancel = () => new InngestTestEngine({
      function: createCancelServerUpgrade(new Inngest({ id: "test" }), runEffect),
      events: [{ name: "inngest/function.cancelled", data: { function_id: "roll-out-server-upgrade", run_id: "run-1" } }],
    }).execute();

    expect((await cancel()).result).toEqual({ closed: 1 });
    expect((await cancel()).result).toEqual({ closed: 0 });
    expect(await rows()).toMatchObject([{ outcome: "unknown", stage: "restarting" }]);
    expect(captured.map(({ event }) => event)).toEqual(["server_upgrade_unknown"]);
  });

  it("a plain member can Upgrade, and reads each Server's latest attempt", async () => {
    await runEffect(requestServerUpgrade({ userId }, { organizationSlug: "acme", machineId }));
    expect(send.mock.calls).toEqual([[{ name: "server/upgrade.requested", data: { organizationId, machineId, trigger: "manual", userId } }]]);

    await rollOut();
    expect(await runEffect(listLatestServerUpgrades({ userId }, { organizationSlug: "acme" }))).toEqual({
      [machineId]: {
        attemptId: requests[0]?.attemptId,
        outcome: "succeeded",
        stage: null,
        error: null,
        fromVersion: "0.2.1",
        targetVersion: "0.2.2",
        startedAt: expect.any(String),
      },
    });
  });
});
