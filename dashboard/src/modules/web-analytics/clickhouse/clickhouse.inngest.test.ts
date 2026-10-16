import { InngestTestEngine } from "@inngest/test";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { beforeEach, expect, it, vi } from "vitest";
import { makeEffectRunner, makeInngestEffectRunner } from "#/server/run.server";
import { silentClickHouse } from "#/test/clickhouse";
import * as postgresRollup from "../postgres/rollup";
import { ClickHouse, type ClickHouseService } from "./client";
import { createClickHouseFunctions } from "./clickhouse.inngest";
import * as promote from "./promote";
import * as rollup from "./rollup";
import * as attempt from "./attempt";
import * as writer from "./writer";

const SHOP = "00000000-0000-4000-8000-00000000c4b1";
const queued: Array<number> = [];
vi.spyOn(writer, "drainOutbox").mockImplementation(() => Effect.sync(() => queued.shift() ?? 0));
vi.spyOn(writer, "ensureBackfilled").mockImplementation(() => Effect.succeed(2));
vi.spyOn(postgresRollup, "rollupSites").mockImplementation(() => Effect.succeed([SHOP]));
vi.spyOn(rollup, "closeDays").mockImplementation(() => Effect.succeed(1));
vi.spyOn(promote, "promoteReads").mockImplementation(() => Effect.succeed(false));
vi.spyOn(attempt, "pin").mockImplementation(() => Effect.succeed(5));
vi.spyOn(attempt, "collectGarbage").mockImplementation(() => Effect.succeed(3));

beforeEach(() => {
  queued.length = 0;
  vi.clearAllMocks();
});

const functions = (clickhouse: ClickHouseService | null) => {
  const run = makeEffectRunner<never, never>({ runPromiseExit: (effect) => Effect.runPromiseExit(Effect.provideService(effect, ClickHouse, clickhouse)) });
  const [drain, rollupFunction, , , gc] = createClickHouseFunctions(new Inngest({ id: "clickhouse-test" }), makeInngestEffectRunner(run) as Parameters<typeof createClickHouseFunctions>[1]);
  if (drain === undefined || rollupFunction === undefined || gc === undefined) throw new Error("missing ClickHouse functions");
  return { drain: new InngestTestEngine({ function: drain }), rollup: new InngestTestEngine({ function: rollupFunction }), gc: new InngestTestEngine({ function: gc }) };
};

it("without ClickHouse the drain stops before touching the outbox", async () => {
  queued.push(writer.DRAIN_BATCHES);

  await functions(null).drain.execute();

  expect(writer.drainOutbox).not.toHaveBeenCalled();
});

it("the drain keeps taking full batches and stops at the first short one", async () => {
  queued.push(writer.DRAIN_BATCHES, writer.DRAIN_BATCHES, 3, writer.DRAIN_BATCHES);

  await functions(silentClickHouse).drain.execute();

  expect(queued).toEqual([writer.DRAIN_BATCHES]);
});

it("the rollup backfills a site's ClickHouse copy before closing its days, with the clock read inside a step", async () => {
  const { state } = await functions(silentClickHouse).rollup.execute();

  expect(writer.ensureBackfilled).toHaveBeenCalledWith(silentClickHouse, SHOP);
  const [backfill] = vi.mocked(writer.ensureBackfilled).mock.invocationCallOrder;
  const [close] = vi.mocked(rollup.closeDays).mock.invocationCallOrder;
  expect(backfill).toBeLessThan(close ?? 0);
  expect(rollup.closeDays).toHaveBeenCalledWith(SHOP, expect.any(Date));
  expect(Object.keys(state)).toHaveLength(["now", "sites", "backfill", "close-days", "promote"].length);
});

it("garbage collection takes a pin, waits out a grace period, then fences and collects below that pin", async () => {
  const { state } = await functions(silentClickHouse).gc.execute({ steps: [{ id: "grace", handler: () => undefined }] });

  expect(attempt.collectGarbage).toHaveBeenCalledWith(silentClickHouse, 5);
  expect(Object.keys(state)).toHaveLength(["pin", "grace", "collect"].length);
});
