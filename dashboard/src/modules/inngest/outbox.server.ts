import "@tanstack/react-start/server-only";
import { and, asc, eq, inArray, isNull, lt, sql } from "drizzle-orm";
import { Effect } from "effect";
import { InngestClient, InngestEventSendError, type InngestSendableEvent, type PloyzInngest } from "#/modules/inngest/client";
import { operationOutbox } from "#/modules/inngest/outbox.tables";
import { afterDatabaseCommit, Database } from "#/server/database.server";
import { runInngestEffect } from "#/modules/inngest/run.server";
import { checkedNow } from "#/db/tables";

/** An event Inngest dedupes by its id, so sending it twice starts one run. */
export type OutboxEvent = InngestSendableEvent & { readonly id: string };
type OutboxRow = typeof operationOutbox.$inferSelect;

const BATCH = 100;
// Younger rows are still in their own commit's delivery.
const SWEEP_AFTER_MS = 30_000;

const eventOf = ({ eventId, name, data }: OutboxRow) => ({ id: eventId, name, data });

const send = Effect.fn("Outbox.send")(function* (rows: readonly OutboxRow[]) {
  const [first, ...rest] = rows;
  if (first === undefined) return;
  const inngest = yield* InngestClient;
  yield* Effect.tryPromise({
    try: () => inngest.send(rest.length === 0 ? eventOf(first) : rows.map(eventOf)),
    catch: (cause) => new InngestEventSendError({ cause }),
  });
  const { drizzle } = yield* Database;
  yield* drizzle.update(operationOutbox).set({ sentAt: sql`now()` })
    .where(inArray(operationOutbox.eventId, rows.map((row) => row.eventId)));
});

const deliver = (eventId: string) =>
  Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const rows = yield* drizzle.select().from(operationOutbox)
      .where(and(eq(operationOutbox.eventId, eventId), isNull(operationOutbox.sentAt)));
    yield* send(rows);
  }).pipe(Effect.catchCause((cause) => Effect.logWarning("An outbox event waits for the sweep.", cause)));

/**
 * Writes `event` in the caller's transaction and sends it once that commits. A send that fails leaves the row for
 * the sweep; the request stays committed either way. `detach` answers without waiting for the send.
 */
export const enqueueEvent = Effect.fn("Outbox.enqueue")(function* (event: OutboxEvent, delivery: "await" | "detach" = "await") {
  const { drizzle } = yield* Database;
  yield* drizzle.insert(operationOutbox).values({ eventId: event.id, name: event.name, data: event.data }).onConflictDoNothing();
  yield* afterDatabaseCommit(delivery === "detach" ? Effect.forkDetach(deliver(event.id)) : deliver(event.id));
});

/** Sends the events whose own delivery failed; returns how many went. */
export const sweepOutbox = Effect.fn("Outbox.sweep")(function* () {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select().from(operationOutbox)
    .where(and(isNull(operationOutbox.sentAt), lt(operationOutbox.createdAt, sql`${checkedNow()} - make_interval(secs => ${SWEEP_AFTER_MS / 1000})`)))
    .orderBy(asc(operationOutbox.createdAt))
    .limit(BATCH);
  yield* send(rows);
  return rows.length;
});

export const createSweepOperationOutbox = (inngest: PloyzInngest) =>
  inngest.createFunction(
    { id: "sweep-operation-outbox", retries: 1, triggers: [{ cron: "* * * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => ({ sent: await step.run("send-unsent", () => runInngestEffect(sweepOutbox())) }),
  );
