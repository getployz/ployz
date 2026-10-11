import { Effect } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { InngestClient } from "#/modules/inngest/client";
import { createVolumeRunRequestedEvent } from "#/modules/inngest/events";
import { enqueueEvent, sweepOutbox } from "#/modules/operations/outbox.server";
import { Database } from "#/server/database.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const inngest = new Inngest({ id: "outbox-test" });
const send = vi.spyOn(inngest, "send");
const event = (runId: string) =>
  createVolumeRunRequestedEvent({ organizationId: "org", environment: "env", volumeId: "vol", runId });

describe("operation outbox", () => {
  let harness: PostgresTestHarness;
  const run = <A, E>(effect: Effect.Effect<A, E, Database | InngestClient>) =>
    harness.runEffect(effect.pipe(Effect.provideService(InngestClient, inngest)));
  const enqueue = (runId: string) =>
    run(Effect.flatMap(Database, (database) => database.transaction(enqueueEvent(event(runId)))));
  const unsent = async () =>
    (await harness.pool.query(`select event_id from operation_outbox where sent_at is null order by event_id`)).rows
      .map(({ event_id }: { event_id: string }) => event_id);

  beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
  afterAll(async () => { await harness?.stop(); });
  beforeEach(async () => {
    send.mockReset();
    send.mockResolvedValue({ ids: [] });
    await harness.pool.query(`truncate table operation_outbox;`);
  });

  it("sends an event once its commit lands, and the sweep finds nothing left", async () => {
    await enqueue("a");

    expect(send.mock.calls).toEqual([[event("a")]]);
    expect(await unsent()).toEqual([]);
    expect(await run(sweepOutbox(new Date(Date.now() + 60_000)))).toBe(0);
  });

  it("the sweep sends what a failed delivery left, only once it is old enough, and marks it sent", async () => {
    send.mockRejectedValueOnce(new Error("inngest down")).mockRejectedValueOnce(new Error("inngest down"));
    await enqueue("a");
    await enqueue("b");
    expect(await unsent()).toEqual(["volume-run-a", "volume-run-b"]);

    expect(await run(sweepOutbox(new Date()))).toBe(0);
    expect(await run(sweepOutbox(new Date(Date.now() + 60_000)))).toBe(2);

    expect(send.mock.calls.at(-1)).toEqual([[event("a"), event("b")]]);
    expect(await unsent()).toEqual([]);
  });
});
