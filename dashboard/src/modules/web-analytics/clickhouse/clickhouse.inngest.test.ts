import { InngestTestEngine } from "@inngest/test";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { beforeEach, expect, it, vi } from "vitest";
import { makeEffectRunner, makeInngestEffectRunner } from "#/server/run.server";
import * as postgresRollup from "../postgres/rollup";
import { ClickHouse, type ClickHouseService } from "./client";
import { createClickHouseFunctions } from "./clickhouse.inngest";
import * as promote from "./promote";
import * as rollup from "./rollup";
import * as writer from "./writer";

const SHOP = "00000000-0000-4000-8000-00000000c4b1";
const fake: ClickHouseService = { query: () => Effect.succeed([]), command: () => Effect.void, insert: () => Effect.void };
const queued: Array<number> = [];
vi.spyOn(writer, "drainOutbox").mockImplementation(() => Effect.sync(() => queued.shift() ?? 0));
vi.spyOn(writer, "ensureBackfilled").mockImplementation(() => Effect.succeed(2));
vi.spyOn(postgresRollup, "rollupSites").mockImplementation(() => Effect.succeed([SHOP]));
vi.spyOn(rollup, "closeDays").mockImplementation(() => Effect.succeed(1));
vi.spyOn(promote, "promoteReads").mockImplementation(() => Effect.succeed(false));
const pins: Array<writer.Pinned> = [{ environmentId: SHOP, timezone: "UTC", copy: 2, floor: 1, generation: 4 }];
vi.spyOn(writer, "publishedPins").mockImplementation(() => Effect.succeed(pins));
vi.spyOn(writer, "collectGarbage").mockImplementation(() => Effect.succeed(3));

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

  await functions(fake).drain.execute();

  expect(queued).toEqual([writer.DRAIN_BATCHES]);
});

it("the rollup backfills a site's ClickHouse copy before closing its days, with the clock read inside a step", async () => {
  const { state } = await functions(fake).rollup.execute();

  expect(writer.ensureBackfilled).toHaveBeenCalledWith(fake, SHOP);
  const [backfill] = vi.mocked(writer.ensureBackfilled).mock.invocationCallOrder;
  const [close] = vi.mocked(rollup.closeDays).mock.invocationCallOrder;
  expect(backfill).toBeLessThan(close ?? 0);
  expect(rollup.closeDays).toHaveBeenCalledWith(SHOP, expect.any(Date));
  expect(Object.keys(state)).toHaveLength(["now", "sites", "backfill", "close-days", "promote"].length);
});

it("garbage collection pins what each site publishes, waits out a grace period, then collects against those pins", async () => {
  const { state } = await functions(fake).gc.execute({ steps: [{ id: "grace", handler: () => undefined }] });

  expect(writer.collectGarbage).toHaveBeenCalledWith(fake, pins);
  expect(Object.keys(state)).toHaveLength(["pin", "grace", "collect"].length);
});
