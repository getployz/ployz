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
import { createCancelServerUpgrade, createRollOutServerUpgrade, createScheduleServerUpgrades } from "#/modules/server-upgrade/server-upgrade.inngest";
import {
  listLatestServerUpgrades,
  readCliServerUpgrade,
  requestCliServerUpgrade,
  requestServerUpgrade,
  setServerUpgradeSettings,
} from "#/modules/server-upgrade/server-upgrade.server";
import type { Database } from "#/server/database.server";
import { makeInngestEffectRunner, type runInngestEffect } from "#/server/run.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const organizationId = "00000000-0000-4000-8000-000000000c01";
const userId = "00000000-0000-4000-8000-000000000c02";
const machineId = "a".repeat(32);
// Every Server's ID repeats one hex digit: web-1 is `1…1`, web-2 `2…2`, web-10 `a…a` above.
const serverId = (digit: string) => digit.repeat(32);
/** The newest release the stable pointer names; the beta pointer names `publishedBeta`, or the same with none. */
let published = "0.2.2";
let publishedBeta: string | null = null;
const release = vi.spyOn(globalThis, "fetch").mockImplementation(async (url) =>
  new Response(`v${String(url).endsWith("/beta") ? publishedBeta ?? published : published}\n`));
const inngest = new Inngest({ id: "server-upgrade-test" });
const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

type Attempt = MachineUpgradeAttempt;
const running = (stage: string): Partial<Attempt> => ({ outcome: "running", stage });

describe("roll-out-server-upgrade", () => {
  let harness: PostgresTestHarness;
  let frame: RuntimeWatchView;
  /** What the fake Server answers the request with; `busy` refuses it as Busy, `unreachable` never answers. */
  let requestAnswer: Partial<Attempt> | "busy" | "unreachable";
  /** What each inspect answers, in order; the last repeats. `unreadable` fails like a restarting daemon. */
  let inspectAnswers: Array<Partial<Attempt> | "unreadable">;
  /** Per-Server answers, over the two above. */
  let requestAnswerFor: Record<string, Partial<Attempt> | "busy">;
  let inspectAnswersFor: Record<string, Array<Partial<Attempt> | "unreadable">>;
  /** After this many inspects, every attempt's `startedAt` moves past the observation limit; null never. */
  let outliveAfterInspects: number | null;
  let inspects: number;
  let requests: Array<{ machine: string; attemptId: string; release: string }>;
  /** Each request and each terminal answer, in the order the Servers saw them. */
  let log: string[];
  let captured: Parameters<PostHogService["capture"]>[0][];
  let identified: Array<Parameters<PostHogService["identify"]>>;

  const attempt = (attemptId: string, answer: Partial<Attempt>) =>
    ({ attempt_id: attemptId as MachineUpgradeAttemptId, target: published, ...answer }) as Attempt;
  const runEffect = makeInngestEffectRunner(<A, E>(operation: Effect.Effect<A, E, Database | OrganizationRuntime | InngestClient>) =>
    harness.runEffect(operation.pipe(
      Effect.provideService(InngestClient, inngest),
      Effect.provideService(PostHog, {
        capture: (input) => Effect.sync(() => void captured.push(input)),
        identify: (distinctId, properties) => Effect.sync(() => void identified.push([distinctId, properties])),
        identifyOrganization: () => Effect.void,
      }),
      Effect.provideService(OrganizationRuntime, {
        cancel: () => Effect.void,
        open: () => Effect.succeed({ status: "connected" as const, connected: asTestDouble<PloyzSession>()({
          watchFirstFrame: () => Effect.succeed(frame),
          requestMachineUpgrade: (machine: string, attemptId: string, release: string) => {
            requests.push({ machine, attemptId, release });
            log.push(`request ${machine}`);
            const answer = requestAnswerFor[machine] ?? requestAnswer;
            if (answer === "unreachable") {
              return Effect.fail(new PloyzProviderError({ operation: "request machine upgrade", cause: { code: "unavailable" } }));
            }
            return answer === "busy"
              ? Effect.fail(new PloyzProviderError({ operation: "request machine upgrade", cause: { code: "conflict", message: "busy" } }))
              : Effect.succeed(attempt(attemptId, answer));
          },
          inspectMachineUpgrade: (machine: string, attemptId: string) => {
            inspects += 1;
            const outlive = inspects === outliveAfterInspects
              ? Effect.promise(() => harness.pool.query("update server_upgrade_attempt set started_at = started_at - interval '20 minutes'"))
              : Effect.void;
            const answers = inspectAnswersFor[machine] ??= [...inspectAnswers];
            const answer = answers.length > 1 ? answers.shift() : answers[0];
            if (answer === undefined || answer === "unreadable") {
              return Effect.andThen(outlive, Effect.fail(new PloyzProviderError({ operation: "inspect machine upgrade", cause: { code: "unavailable" } })));
            }
            if (answer.outcome !== "running") log.push(`${answer.outcome} ${machine}`);
            return Effect.andThen(outlive, Effect.succeed(attempt(attemptId, answer)));
          },
        }) }),
      }),
    ))) as typeof runInngestEffect;

  /** Upgrade on a Server page names its Server; Upgrade and Upgrade the rest on the Servers page name none. */
  const rollOut = (target: string | null = machineId, trigger: "manual" | "automatic" = "manual") =>
    rollOutRequested({ organizationId, machineId: target, trigger, userId: trigger === "manual" ? userId : null });
  const rollOutRequested = (data: Record<string, unknown>) => new InngestTestEngine({
    function: createRollOutServerUpgrade(new Inngest({ id: "test" }), runEffect),
    events: [{ name: "server/upgrade.requested", data }],
    // Each poll's sleep ends at once; `outliveAfterInspects` moves the clock instead.
    steps: frame.machines.flatMap(({ machine }) =>
      Array.from({ length: 80 }, (_, poll) => ({ id: `wait-${machine.id}-${poll}`, handler: () => undefined }))),
  }).execute();
  const server = (digit: string, name: string, version: string, extra: { membership?: string; runningBuilds?: number } = {}) => {
    const machine = runtimeWatchMachineFixture(serverId(digit), name);
    machine.runtime = { ...machine.runtime, daemon_version: version, running_builds: extra.runningBuilds ?? 0 };
    return runtimeWatchMachineObservationFixture({ machine, membership: extra.membership ?? "up" });
  };
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
    requestAnswerFor = {};
    inspectAnswersFor = {};
    outliveAfterInspects = null;
    inspects = 0;
    requests = [];
    log = [];
    captured = [];
    identified = [];
    published = "0.2.2";
    publishedBeta = null;
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
    expect(output.result).toMatchObject({ results: [{ machineId, outcome: "succeeded" }] });
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

    expect(output.result).toMatchObject({ results: [{ outcome: "failed" }] });
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

  it("records unknown with the last stage seen twenty minutes after the attempt started without an outcome", async () => {
    inspectAnswers = [running("readiness"), "unreadable"];
    outliveAfterInspects = 3;

    const output = await rollOut();

    expect(output.result).toMatchObject({ results: [{ outcome: "unknown" }] });
    expect(inspects).toBe(3);
    expect(await rows()).toMatchObject([{ outcome: "unknown", stage: "readiness", error: null }]);
    expect(captured).toEqual([expect.objectContaining({ event: "server_upgrade_unknown", properties: expect.objectContaining({ stage: "readiness" }) })]);
  });

  it("records unknown, as the sweep and the Server page read it, for an outcome that arrives after twenty minutes", async () => {
    inspectAnswers = [running("readiness"), { outcome: "succeeded", version: "0.2.2" }];
    outliveAfterInspects = 1;

    const output = await rollOut();

    expect(output.result).toMatchObject({ results: [{ outcome: "unknown" }] });
    expect(inspects).toBe(1);
    expect(await rows()).toMatchObject([{ outcome: "unknown", stage: "readiness" }]);
  });

  it("records nothing and sends nothing when the Server refuses as Busy", async () => {
    requestAnswer = "busy";

    const output = await rollOut();

    expect(output.result).toEqual({ results: [{ machineId, kind: "skipped", reason: "busy" }] });
    expect(await rows()).toEqual([]);
    expect(captured).toEqual([]);
  });

  it("requests nothing for a Server that is building", async () => {
    const observed = frame.machines[0];
    if (observed) observed.machine.runtime.running_builds = 1;

    expect((await rollOut()).result).toEqual({ results: [{ machineId, kind: "skipped", reason: "not-online" }] });
    expect(requests).toEqual([]);
    expect(await rows()).toEqual([]);
  });

  describe("every Server behind, from the Servers page", () => {
    beforeEach(() => {
      frame = runtimeWatchFrameFixture({ machines: [
        server("a", "web-10", "0.2.1"),
        server("2", "web-2", "0.2.1"),
        server("5", "web-5", "0.2.2"),
        server("1", "web-1", "0.2.1"),
      ] });
    });

    it("upgrades them one at a time, in name order, along stable", async () => {
      const output = await rollOut(null);

      expect(output.error).toBeUndefined();
      expect(log).toEqual([
        `request ${serverId("1")}`, `succeeded ${serverId("1")}`,
        `request ${serverId("2")}`, `succeeded ${serverId("2")}`,
        `request ${serverId("a")}`, `succeeded ${serverId("a")}`,
      ]);
      expect(new Set(requests.map(({ release }) => release))).toEqual(new Set(["stable"]));
      expect((await rows()).map(({ outcome }) => outcome)).toEqual(["succeeded", "succeeded", "succeeded"]);
      expect(captured.map(({ event }) => event)).toEqual(Array(3).fill("server_upgrade_succeeded"));
      expect(release).toHaveBeenCalledWith("https://ployz.sh/v0/stable", expect.anything());
    });

    it("stops at the first outcome that isn't succeeded, so the rest aren't attempted", async () => {
      inspectAnswersFor[serverId("2")] = [{ outcome: "failed", stage: "readiness", error: "readiness timed out; restored 0.2.1" }];

      const output = await rollOut(null);

      expect(log).toEqual([
        `request ${serverId("1")}`, `succeeded ${serverId("1")}`,
        `request ${serverId("2")}`, `failed ${serverId("2")}`,
      ]);
      expect(output.result).toMatchObject({ results: [{ outcome: "succeeded" }, { outcome: "failed" }] });
      expect(captured.map(({ event }) => event)).toEqual(["server_upgrade_succeeded", "server_upgrade_failed"]);
    });

    it("skips offline and building Servers, which stay behind", async () => {
      frame = runtimeWatchFrameFixture({ machines: [
        server("1", "web-1", "0.2.1", { membership: "down" }),
        server("2", "web-2", "0.2.1", { runningBuilds: 1 }),
        server("3", "web-3", "0.2.1"),
      ] });

      await rollOut(null);

      expect(requests.map(({ machine }) => machine)).toEqual([serverId("3")]);
      expect(await rows()).toHaveLength(1);
    });

    it("skips a Server that refuses as Busy, with no row and no event, and goes on", async () => {
      requestAnswerFor[serverId("1")] = "busy";

      const output = await rollOut(null);

      expect(requests.map(({ machine }) => machine)).toEqual([serverId("1"), serverId("2"), serverId("a")]);
      expect(output.result).toMatchObject({ results: [{ machineId: serverId("1"), kind: "skipped", reason: "busy" }, { outcome: "succeeded" }, { outcome: "succeeded" }] });
      expect(await rows()).toHaveLength(2);
      expect(captured).toHaveLength(2);
    });

    it("never runs two rollouts at once in one Organization: overlapping requests queue", () => {
      // Inngest runs one rollout per Organization and queues the rest; each run upgrades one Server at a time.
      expect(createRollOutServerUpgrade(new Inngest({ id: "test" }), runEffect).opts.concurrency)
        .toEqual([{ key: "event.data.organizationId", limit: 1 }]);
    });
  });

  describe("automatic upgrades", () => {
    const failure = { outcome: "failed", stage: "readiness", error: "readiness timed out; restored 0.2.1" } as const;
    const schedule = () => new InngestTestEngine({
      function: createScheduleServerUpgrades(new Inngest({ id: "test" }), runEffect),
      steps: [{ id: "request-automatic-rollouts", handler: () => ({ ids: [] }) }],
    }).execute();

    beforeEach(() => {
      frame = runtimeWatchFrameFixture({ machines: [server("1", "web-1", "0.2.1"), server("2", "web-2", "0.2.1"), server("3", "web-3", "0.2.1")] });
    });

    /** web-1 upgraded and web-2 failed (and restored), so the rollout halted on 0.2.2 before web-3. */
    const halt = async () => {
      inspectAnswersFor[serverId("2")] = [failure];
      await rollOut(null, "automatic");
      frame = runtimeWatchFrameFixture({ machines: [server("1", "web-1", "0.2.2"), server("2", "web-2", "0.2.1"), server("3", "web-3", "0.2.1")] });
      inspectAnswersFor = {};
      requests = [];
      captured = [];
    };

    it("the hourly schedule requests one automatic rollout per paired Organization that turned automatic upgrades on, off by default", async () => {
      const org = (digit: string) => `00000000-0000-4000-8000-00000000000${digit}`;
      await harness.pool.query(`
        insert into organization (id, name, slug) values
          ('${org("1")}', 'Off', 'off'), ('${org("2")}', 'On', 'on'), ('${org("3")}', 'Unpaired', 'unpaired');
        insert into organization_server_upgrades (organization_id, automatic) values ('${org("1")}', false), ('${org("2")}', true);
        insert into organization_pairing (organization_id, encrypted_pairing_secret, founder_claim_machine_id, founder_machine_id) values
          ('${organizationId}', '{}', '${serverId("1")}', '${serverId("1")}'),
          ('${org("1")}', '{}', '${serverId("1")}', '${serverId("1")}'),
          ('${org("2")}', '{}', '${serverId("1")}', '${serverId("1")}');
      `);
      const fn = createScheduleServerUpgrades(new Inngest({ id: "test" }), runEffect);
      expect(fn.opts.triggers).toEqual([{ cron: "TZ=UTC 0 * * * *" }]);

      const output = await schedule();

      expect(output.ctx.step.sendEvent).toHaveBeenCalledTimes(1);
      const [[id, events]] = vi.mocked(output.ctx.step.sendEvent).mock.calls as [[string, unknown[]]];
      expect(id).toBe("request-automatic-rollouts");
      expect(events).toEqual([{
        name: "server/upgrade.requested",
        data: { organizationId: org("2"), machineId: null, trigger: "automatic", userId: null },
      }]);
    });

    it("a plain member turns automatic upgrades off and on, and the schedule follows", async () => {
      await harness.pool.query(`
        insert into organization_pairing (organization_id, encrypted_pairing_secret, founder_claim_machine_id, founder_machine_id)
        values ('${organizationId}', '{}', '${serverId("1")}', '${serverId("1")}');
      `);
      const requested = async () => (await schedule()).result;

      expect(await runEffect(setServerUpgradeSettings({ userId }, { organizationSlug: "acme", automatic: false })))
        .toEqual({ id: organizationId, automatic: false, channel: "stable" });
      expect(await requested()).toMatchObject({ organizationCount: 0 });
      await runEffect(setServerUpgradeSettings({ userId }, { organizationSlug: "acme", automatic: true }));
      expect(await requested()).toMatchObject({ organizationCount: 1 });
    });

    it("saving automatic upgrades on starts a rollout now, with or without a Cluster; off or a channel change sends nothing", async () => {
      const save = (settings: { automatic?: boolean; channel?: "stable" | "beta" }) =>
        runEffect(setServerUpgradeSettings({ userId }, { organizationSlug: "acme", ...settings }));
      const automatic = { name: "server/upgrade.requested", data: { organizationId, machineId: null, trigger: "automatic", userId: null } };
      await save({ automatic: false });
      await save({ channel: "beta" });
      expect(send).not.toHaveBeenCalled();
      await save({ automatic: true });

      await harness.pool.query(`
        insert into organization_pairing (organization_id, encrypted_pairing_secret, founder_claim_machine_id, founder_machine_id)
        values ('${organizationId}', '{}', '${serverId("1")}', '${serverId("1")}');
      `);
      await save({ automatic: false });
      await save({ automatic: true });
      expect(send.mock.calls).toEqual([[automatic], [automatic]]);
    });

    it("an automatic rollout halts at the first non-success, and the halt holds across hourly runs", async () => {
      inspectAnswersFor[serverId("2")] = [failure];

      await rollOut(null, "automatic");

      expect(requests.map(({ machine }) => machine)).toEqual([serverId("1"), serverId("2")]);
      expect((await rows()).map(({ trigger, requested_by_user_id: user }) => [trigger, user]))
        .toEqual([["automatic", null], ["automatic", null]]);

      frame = runtimeWatchFrameFixture({ machines: [server("1", "web-1", "0.2.2"), server("2", "web-2", "0.2.1"), server("3", "web-3", "0.2.1")] });
      requests = [];
      const next = await rollOut(null, "automatic");
      await rollOut(null, "automatic");

      expect(next.result).toEqual({ results: [] });
      expect(requests).toEqual([]);
      expect(await rows()).toHaveLength(2);
    });

    it("an attempt whose request was never answered halts the release it expected, and is closed once", async () => {
      requestAnswer = "unreachable";
      await rollOut(null, "automatic");

      expect(await rows()).toMatchObject([{ outcome: "running", target_version: "0.2.2" }]);
      await harness.pool.query("update server_upgrade_attempt set started_at = now() - interval '21 minutes'");
      expect((await schedule()).result).toMatchObject({ closed: 1 });
      requestAnswer = { outcome: "accepted" };
      requests = [];
      await rollOut(null, "automatic");
      expect((await schedule()).result).toMatchObject({ closed: 0 });

      expect(requests).toEqual([]);
      expect(await rows()).toMatchObject([{ outcome: "unknown", target_version: "0.2.2" }]);
      expect(captured.map(({ event, properties }) => [event, properties?.["to_version"]])).toEqual([["server_upgrade_unknown", "0.2.2"]]);
    });

    it("a newer release lifts the halt", async () => {
      // Its own release line, so the pointer this test moves stays out of the other tests' pointer cache.
      published = "1.0.1";
      frame = runtimeWatchFrameFixture({ machines: [server("1", "web-1", "1.0.0"), server("2", "web-2", "1.0.0"), server("3", "web-3", "1.0.0")] });
      inspectAnswersFor[serverId("2")] = [failure];
      await rollOut(null, "automatic");
      frame = runtimeWatchFrameFixture({ machines: [server("1", "web-1", "1.0.1"), server("2", "web-2", "1.0.0"), server("3", "web-3", "1.0.0")] });
      inspectAnswersFor = {};
      requests = [];
      await rollOut(null, "automatic");
      expect(requests).toEqual([]);

      published = "1.0.2";
      // Past Cloud's few minutes of pointer cache.
      const clock = vi.spyOn(Date, "now").mockReturnValue(Date.now() + 6 * 60_000);
      await rollOut(null, "automatic");
      clock.mockRestore();

      expect(requests.map(({ machine }) => machine)).toEqual([serverId("1"), serverId("2"), serverId("3")]);
    });

    it("a successful manual Upgrade of the halted release lifts the halt", async () => {
      await halt();
      await rollOut(serverId("2"));
      frame = runtimeWatchFrameFixture({ machines: [server("1", "web-1", "0.2.2"), server("2", "web-2", "0.2.2"), server("3", "web-3", "0.2.1")] });
      requests = [];

      await rollOut(null, "automatic");

      expect(requests.map(({ machine }) => machine)).toEqual([serverId("3")]);
    });

    it("a manual Upgrade of the rest still runs while halted", async () => {
      await halt();

      await rollOut(null);

      expect(requests.map(({ machine }) => machine)).toEqual([serverId("2"), serverId("3")]);
    });

    it("an Organization on Beta requests beta; on Stable again, Servers ahead of the stable pointer are left alone", async () => {
      // Its own release line, so the pointers this test sets stay out of the other tests' pointer cache.
      published = "3.0.0";
      publishedBeta = "3.0.1-beta.1";
      const beta = { outcome: "succeeded", version: publishedBeta, target: publishedBeta } as const;
      requestAnswer = { outcome: "accepted", target: publishedBeta };
      inspectAnswers = [beta];
      frame = runtimeWatchFrameFixture({ machines: [
        server("1", "web-1", "3.0.0"), server("2", "web-2", "3.0.0"), server("3", "web-3", "3.0.0-beta.2", { membership: "down" }),
      ] });
      await runEffect(setServerUpgradeSettings({ userId }, { organizationSlug: "acme", channel: "beta" }));

      await rollOut(null, "automatic");

      expect(requests.map(({ machine, release: channel }) => [machine, channel])).toEqual([[serverId("1"), "beta"], [serverId("2"), "beta"]]);
      expect((await rows()).map(({ channel, target_version: target }) => [channel, target])).toEqual(Array(2).fill(["beta", publishedBeta]));
      expect(captured.map(({ properties }) => properties?.["channel"])).toEqual(["beta", "beta"]);

      expect(await runEffect(setServerUpgradeSettings({ userId }, { organizationSlug: "acme", channel: "stable" })))
        .toEqual({ id: organizationId, automatic: false, channel: "stable" });
      frame = runtimeWatchFrameFixture({ machines: [
        server("1", "web-1", publishedBeta), server("2", "web-2", publishedBeta), server("3", "web-3", "3.0.0-beta.2"),
      ] });
      requests = [];
      requestAnswer = { outcome: "accepted" };
      inspectAnswers = [{ outcome: "succeeded", version: "3.0.0" }];

      await rollOut(null, "automatic");

      expect(requests.map(({ machine, release: channel }) => [machine, channel])).toEqual([[serverId("3"), "stable"]]);
    });

    it("credits automatic attempts to the Organization's system person, and manual ones to who clicked", async () => {
      await rollOut(serverId("1"), "automatic");
      await rollOut(serverId("2"));

      expect(captured.map(({ userId: distinctId, organizationId: group }) => [distinctId, group])).toEqual([
        [`system:${organizationId}`, organizationId],
        [userId, organizationId],
      ]);
      expect(identified).toEqual([[`system:${organizationId}`, { name: "Acme (system)", is_system: true }]]);
    });

    it("the hourly run closes an attempt still running after twenty minutes as unknown, and sends its event once", async () => {
      await harness.pool.query(`
        insert into server_upgrade_attempt (organization_id, machine_id, attempt_id, trigger, channel, from_version, stage, inngest_run_id, started_at)
        values
          ('${organizationId}', '${serverId("1")}', '${"b".repeat(32)}', 'automatic', 'stable', '0.2.1', 'restarting', 'run-1', now() - interval '21 minutes'),
          ('${organizationId}', '${serverId("2")}', '${"c".repeat(32)}', 'automatic', 'stable', '0.2.1', 'restarting', 'run-2', now() - interval '5 minutes');
      `);

      expect((await schedule()).result).toMatchObject({ closed: 1 });
      expect((await schedule()).result).toMatchObject({ closed: 0 });

      expect((await rows()).map(({ attempt_id: id, outcome, stage }) => [id, outcome, stage])
        .sort(([left], [right]) => String(left).localeCompare(String(right)))).toEqual([
        ["b".repeat(32), "unknown", "restarting"],
        ["c".repeat(32), "running", "restarting"],
      ]);
      expect(captured).toEqual([expect.objectContaining({
        userId: `system:${organizationId}`,
        event: "server_upgrade_unknown",
        properties: expect.objectContaining({ trigger: "automatic", stage: "restarting" }),
      })]);
    });
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

  it("the cancel handler leaves attempts alone when another function's run is cancelled", async () => {
    await harness.pool.query(`
      insert into server_upgrade_attempt (organization_id, machine_id, attempt_id, trigger, channel, from_version, inngest_run_id, started_at)
      values ('${organizationId}', '${machineId}', '${"b".repeat(32)}', 'manual', 'stable', '0.2.1', 'run-1', now());
    `);
    const output = await new InngestTestEngine({
      function: createCancelServerUpgrade(new Inngest({ id: "test" }), runEffect),
      events: [{ name: "inngest/function.cancelled", data: { function_id: "drain-server", run_id: "run-1" } }],
    }).execute();

    expect(output.result).toEqual({ skipped: true });
    expect(await rows()).toMatchObject([{ outcome: "running" }]);
  });

  it("onFailure closes the failed run's running attempts as unknown, once, and no other run's", async () => {
    const onFailure = createRollOutServerUpgrade(new Inngest({ id: "test" }), runEffect).opts.onFailure;
    if (onFailure === undefined) return expect.fail("a failed run closes its attempts");
    await harness.pool.query(`
      insert into server_upgrade_attempt (organization_id, machine_id, attempt_id, trigger, channel, from_version, stage, inngest_run_id, started_at)
      values ('${organizationId}', '${machineId}', '${"b".repeat(32)}', 'manual', 'stable', '0.2.1', 'restarting', 'run-1', now()),
             ('${organizationId}', '${machineId}', '${"c".repeat(32)}', 'manual', 'stable', '0.2.1', null, 'run-2', now());
    `);

    // SAFETY: the failure handler reads only the failed run's ID.
    const fail = (runId: string) => Promise.resolve(onFailure({ event: { data: { run_id: runId } } } as never));
    await fail("run-1");
    await fail("run-1");

    expect(Object.fromEntries((await rows()).map(({ attempt_id: id, outcome, stage }) => [id[0], [outcome, stage]]))).toEqual({
      b: ["unknown", "restarting"],
      c: ["running", null],
    });
    expect(captured.map(({ event }) => event)).toEqual(["server_upgrade_unknown"]);
  });

  describe("`server upgrade` from the CLI", () => {
    const requestCli = () => runEffect(requestCliServerUpgrade({ organizationId, userId }, { machineId, channel: undefined }));
    const readCli = (attemptId: string) => runEffect(readCliServerUpgrade(organizationId, attemptId));
    /** The run of the event the CLI's request sent. */
    const rollOutSent = () => rollOutRequested((send.mock.calls.at(-1)?.[0] as { data: Record<string, unknown> }).data);
    const requested = async () => {
      const answer = await requestCli();
      if (!answer.ok) throw new Error(answer.refusal.message);
      return answer.id;
    };

    it("reads running from its request on, and the run claims and settles that attempt", async () => {
      const id = await requested();
      expect(await readCli(id)).toEqual({ state: "running" });

      await rollOutSent();

      expect(requests).toEqual([{ machine: machineId, attemptId: id, release: "stable" }]);
      expect(await readCli(id)).toEqual({ state: "finished", outcome: "succeeded", from_version: "0.2.1", target_version: "0.2.2", message: null });
      const claimed = await harness.pool.query("select inngest_run_id from server_upgrade_attempt");
      expect(claimed.rows).toEqual([{ inngest_run_id: expect.stringMatching(/.+/u) }]);
    });

    it("joins the attempt already running on the Server instead of queueing one its run would find busy", async () => {
      const first = await requested();

      expect(await requested()).toBe(first);
      expect(send).toHaveBeenCalledTimes(1);
    });

    it("ends busy at once when its run finds the Server busy or building, and leaves no attempt", async () => {
      requestAnswer = "busy";
      const busy = await requested();
      expect((await rollOutSent()).result).toEqual({ results: [{ machineId, kind: "skipped", reason: "busy" }] });
      expect(await readCli(busy)).toMatchObject({ state: "ended", code: "busy" });

      const building = await requested();
      const observed = frame.machines[0];
      if (observed) observed.machine.runtime.running_builds = 1;
      expect((await rollOutSent()).result).toEqual({ results: [{ machineId, kind: "skipped", reason: "not-online" }] });
      expect(await readCli(building)).toMatchObject({ state: "ended", code: "busy" });
      expect(await rows()).toEqual([]);
    });

    it("refuses unavailable, writing and sending nothing, for a Server that can't take an Upgrade now", async () => {
      const observed = frame.machines[0];
      if (observed) observed.machine.runtime.running_builds = 1;

      expect(await requestCli()).toEqual({
        ok: false,
        refusal: { code: "unavailable", message: "This Server isn't online and idle, so it can't take an Upgrade now.", details: null },
      });
      expect(send).not.toHaveBeenCalled();
      expect(await rows()).toEqual([]);
    });

    it("reads unknown once its attempt outlives the observation limit, and a new request starts its own", async () => {
      const stale = await requested();
      await harness.pool.query("update server_upgrade_attempt set started_at = started_at - interval '20 minutes'");

      expect(await readCli(stale)).toMatchObject({ state: "finished", outcome: "unknown" });
      expect(await requested()).not.toBe(stale);
    });

    it("drops its attempt when the event can't be sent", async () => {
      send.mockRejectedValueOnce(new Error("Inngest is down"));

      await expect(requestCli()).rejects.toThrow();
      expect(await rows()).toEqual([]);
    });
  });

  it("a plain member can Upgrade one Server or every Server behind, and reads each Server's latest attempt", async () => {
    await runEffect(requestServerUpgrade({ userId }, { organizationSlug: "acme", machineId }));
    await runEffect(requestServerUpgrade({ userId }, { organizationSlug: "acme", machineId: null }));
    expect(send.mock.calls).toEqual([
      [{ name: "server/upgrade.requested", data: { organizationId, machineId, trigger: "manual", userId } }],
      [{ name: "server/upgrade.requested", data: { organizationId, machineId: null, trigger: "manual", userId } }],
    ]);

    await rollOut();
    expect(await runEffect(listLatestServerUpgrades({ userId }, { organizationSlug: "acme" }))).toEqual({
      servers: {
        [machineId]: {
          attemptId: requests[0]?.attemptId,
          outcome: "succeeded",
          stage: null,
          error: null,
          fromVersion: "0.2.1",
          targetVersion: "0.2.2",
          startedAt: expect.any(String),
        },
      },
    });
  });
});
