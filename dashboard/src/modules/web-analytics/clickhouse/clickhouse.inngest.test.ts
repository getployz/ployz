import { InngestTestEngine } from "@inngest/test";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { beforeEach, expect, it, vi } from "vitest";
import { makeEffectRunner, makeInngestEffectRunner } from "#/server/run.server";
import { ClickHouse, type ClickHouseService } from "./client";
import { createClickHouseFunctions } from "./clickhouse.inngest";
import * as writer from "./writer";

const fake: ClickHouseService = { query: () => Effect.succeed([]), command: () => Effect.void, insert: () => Effect.void };
const queued: Array<number> = [];
vi.spyOn(writer, "drainOutbox").mockImplementation(() => Effect.sync(() => queued.shift() ?? 0));

beforeEach(() => {
  queued.length = 0;
  vi.mocked(writer.drainOutbox).mockClear();
});

const drainFunction = (clickhouse: ClickHouseService | null) => {
  const run = makeEffectRunner<never, never>({ runPromiseExit: (effect) => Effect.runPromiseExit(Effect.provideService(effect, ClickHouse, clickhouse)) });
  // SAFETY: The drain's effects here need only the ClickHouse reference this runner provides.
  const [drain] = createClickHouseFunctions(new Inngest({ id: "clickhouse-test" }), makeInngestEffectRunner(run) as Parameters<typeof createClickHouseFunctions>[1]);
  if (drain === undefined) throw new Error("no drain function");
  return new InngestTestEngine({ function: drain });
};

it("without ClickHouse the drain stops before touching the outbox", async () => {
  queued.push(writer.DRAIN_BATCHES);

  await drainFunction(null).execute();

  expect(writer.drainOutbox).not.toHaveBeenCalled();
});

it("the drain keeps taking full batches and stops at the first short one", async () => {
  queued.push(writer.DRAIN_BATCHES, writer.DRAIN_BATCHES, 3, writer.DRAIN_BATCHES);

  await drainFunction(fake).execute();

  expect(queued).toEqual([writer.DRAIN_BATCHES]);
});
