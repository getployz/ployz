import { InngestTestEngine } from "@inngest/test";
import { Option } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, expect, it } from "vitest";
import { createCancelRetireOld } from "#/modules/config-store/retire-old.inngest";
import { endCleanup, readDeploymentCleanup, recordCleanupWaiting } from "#/modules/config-store/retire-old.server";
import { configDeploymentRetireRequestedEvent } from "#/modules/inngest/events";
import { makeInngestEffectRunner, type runInngestEffect } from "#/server/run.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const ORGANIZATION = "00000000-0000-4000-8000-00000000af01";
const requested = (deploymentId: string) => ({ organizationId: ORGANIZATION, environmentId: "env", deploymentId });

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

async function until(condition: () => Promise<boolean> | boolean) {
  const deadline = Date.now() + 10_000;
  while (!await condition()) {
    if (Date.now() > deadline) throw new Error("the condition never held");
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

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
