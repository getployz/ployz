import { InngestTestEngine, mockCtx } from "@inngest/test";
import { Effect, Option } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, expect, it, vi } from "vitest";
import { createCancelRetireOld, createRetireOld } from "#/modules/config-store/retire-old.inngest";
import * as cleanupRows from "#/modules/config-store/retire-old.server";
import {
  closeStaleCleanups, endCleanup, readDeploymentCleanup, recordCleanupWaiting,
} from "#/modules/config-store/retire-old.server";
import type { InngestRunStatus } from "#/modules/inngest/client";
import { configDeploymentRetireRequestedEvent } from "#/modules/inngest/events";
import { makeInngestEffectRunner, type runInngestEffect } from "#/server/run.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const ORGANIZATION = "00000000-0000-4000-8000-00000000af01";
const requested = (deploymentId: string) => ({ organizationId: ORGANIZATION, environmentId: "env", deploymentId, number: 1 });

let harness: PostgresTestHarness;

beforeAll(async () => {
  harness = await startPostgresTestHarness();
  await harness.pool.query("insert into organization (id, name, slug) values ($1, 'cleanup', 'cleanup')", [ORGANIZATION]);
}, 60_000);

afterAll(async () => {
  await harness.stop();
});

const cancel = (runId: string, deploymentId: string) => new InngestTestEngine({
  function: createCancelRetireOld(
    new Inngest({ id: "ployz-cloud" }),
    makeInngestEffectRunner(harness.runEffect) as typeof runInngestEffect,
  ),
  events: [{
    name: "inngest/function.cancelled",
    data: {
      function_id: "ployz-cloud-retire-old", run_id: runId,
      event: { name: configDeploymentRetireRequestedEvent, data: requested(deploymentId) },
    },
  }],
}).execute();

const blockedCleanupWrites = async () => (await harness.pool.query(
  `select pid from pg_stat_activity
   where datname = current_database() and query like 'insert into "deployment_cleanup"%' and wait_event_type = 'Lock'`,
)).rowCount;

const until = (condition: () => Promise<boolean> | boolean) => vi.waitFor(async () => {
  if (!await condition()) throw new Error("the condition never held");
}, { timeout: 10_000, interval: 10 });

it("never rewrites how a run ended, and lets a newer run of the same Deployment take the row", async () => {
  await harness.runEffect(recordCleanupWaiting("first", requested("rerun")));
  expect(await harness.runEffect(endCleanup("first", { state: "done" }, Option.some(requested("rerun"))))).toBe(1);
  await cancel("first", "rerun");
  await harness.runEffect(recordCleanupWaiting("first", requested("rerun")));
  expect(await harness.runEffect(readDeploymentCleanup(ORGANIZATION, "rerun"))).toMatchObject({ state: "done" });

  await harness.runEffect(recordCleanupWaiting("second", requested("rerun")));
  await cancel("third", "rerun");
  expect(await harness.runEffect(readDeploymentCleanup(ORGANIZATION, "rerun"))).toMatchObject({ state: "waiting" });
});

it("keeps a cleanup cancelled when the cancel lands while its first waiting write is in flight", async () => {
  const lock = await harness.pool.connect();
  let inserting: Promise<void> | undefined;
  let cancelling: Promise<unknown> | undefined;
  try {
    await lock.query("begin");
    await lock.query("select id from organization where id = $1 for update", [ORGANIZATION]);
    inserting = harness.runEffect(recordCleanupWaiting("race", requested("race")));
    await until(async () => await blockedCleanupWrites() === 1);
    let cancelSettled = false;
    cancelling = cancel("race", "race").finally(() => { cancelSettled = true; });
    await until(async () => cancelSettled || await blockedCleanupWrites() === 2);
  } finally {
    await lock.query("rollback");
    lock.release();
    await inserting;
    await cancelling;
  }
  expect(await harness.runEffect(readDeploymentCleanup(ORGANIZATION, "race"))).toMatchObject({ state: "cancelled" });
}, 30_000);

it("keeps the copies a cleanup last named when a newer Deployment takes it over", async () => {
  const machine3 = { machine: "machine-3", service: "web", reason: "machine-3 did not answer" };
  await harness.runEffect(recordCleanupWaiting("taken", requested("taken"), [machine3]));
  await cancel("taken", "taken");
  expect(await harness.runEffect(readDeploymentCleanup(ORGANIZATION, "taken"))).toEqual({ state: "cancelled", left: [machine3], message: null });
});

it("fails only a waiting cleanup whose Inngest run is gone, keeping what it left, and never a live or unknown one", async () => {
  const machine3 = { machine: "machine-3", service: "web", reason: "machine-3 did not answer" };
  const statuses = new Map<string, InngestRunStatus | "unreachable">([
    ["run-completed", "completed"], ["run-missing", "missing"], ["run-slow", "running"], ["run-unreachable", "unreachable"],
    ["run-fresh", "missing"],
  ]);
  for (const runId of statuses.keys()) await harness.runEffect(recordCleanupWaiting(runId, requested(runId), [machine3]));
  await harness.pool.query("update deployment_cleanup set updated_at = now() - interval '1 hour' where run_id <> 'run-fresh'");

  const swept = await harness.runEffect(closeStaleCleanups((runId) => {
    const status = statuses.get(runId);
    return status === undefined || status === "unreachable" ? Effect.fail(new Error("unreachable")) : Effect.succeed(status);
  }));

  expect(swept).toEqual({ closed: 2 });
  const lost = { state: "failed", left: [machine3], message: "Cloud lost track of this cleanup. Deploy again to retire what it left." };
  const waiting = { state: "waiting", left: [machine3], message: null };
  const states = await Promise.all([...statuses.keys()].map((runId) => harness.runEffect(readDeploymentCleanup(ORGANIZATION, runId))));
  expect(states).toEqual([lost, lost, waiting, waiting, waiting]);
});

it("ends done with nothing left once a retry retires the copy an earlier try left", async () => {
  const machine3 = { machine: "machine-3", service: "web", reason: "machine-3 did not answer" };
  const answers = [[machine3], []];
  const retire = vi.spyOn(cleanupRows, "retireOldCopies").mockImplementation(() => Effect.succeed({ left: answers.shift() ?? [], kept: [] }));
  try {
    await new InngestTestEngine({
      function: createRetireOld(new Inngest({ id: "ployz-cloud" }), makeInngestEffectRunner(harness.runEffect) as typeof runInngestEffect),
      events: [{ name: configDeploymentRetireRequestedEvent, data: requested("retried") }],
      transformCtx: (ctx) => ({ ...mockCtx(ctx), runId: "retried" }),
      steps: [{ id: "pause-1", handler: () => undefined }],
    }).execute();
  } finally {
    retire.mockRestore();
  }
  expect(answers).toEqual([]);
  expect(await harness.runEffect(readDeploymentCleanup(ORGANIZATION, "retried"))).toEqual({ state: "done", left: [], message: null });
});
