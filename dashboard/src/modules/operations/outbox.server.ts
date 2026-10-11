import "@tanstack/react-start/server-only";
import { and, asc, eq, inArray, isNull, lt } from "drizzle-orm";
import { Effect } from "effect";
import { InngestClient, InngestEventSendError, type PloyzInngest } from "#/modules/inngest/client";
import type { InngestSendableEvent } from "#/modules/inngest/events";
import { operationOutbox } from "#/modules/operations/tables";
import { afterDatabaseCommit, Database } from "#/server/database.server";
import { runInngestEffect } from "#/server/run.server";

/** An event Inngest dedupes by its id, so sending it twice starts one run. */
export type OutboxEvent = Extract<InngestSendableEvent, { readonly id: string }>;
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
  yield* drizzle.update(operationOutbox).set({ sentAt: new Date() })
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
 * the sweep; the request stays committed either way.
 */
export const enqueueEvent = Effect.fn("Outbox.enqueue")(function* (event: OutboxEvent) {
  const { drizzle } = yield* Database;
  yield* drizzle.insert(operationOutbox).values({ eventId: event.id, name: event.name, data: event.data }).onConflictDoNothing();
  yield* afterDatabaseCommit(deliver(event.id));
});

/** Sends the events whose own delivery failed; returns how many went. */
export const sweepOutbox = Effect.fn("Outbox.sweep")(function* (now: Date) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select().from(operationOutbox)
    .where(and(isNull(operationOutbox.sentAt), lt(operationOutbox.createdAt, new Date(now.getTime() - SWEEP_AFTER_MS))))
    .orderBy(asc(operationOutbox.createdAt))
    .limit(BATCH);
  yield* send(rows);
  return rows.length;
});

export const createSweepOperationOutbox = (inngest: PloyzInngest, runEffect: typeof runInngestEffect = runInngestEffect) =>
  inngest.createFunction(
    { id: "sweep-operation-outbox", retries: 1, triggers: [{ cron: "* * * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => ({ sent: await step.run("send-unsent", () => runEffect(sweepOutbox(new Date()))) }),
  );
