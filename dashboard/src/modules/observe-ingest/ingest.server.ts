import "@tanstack/react-start/server-only";
import { sql } from "drizzle-orm";
import { Effect } from "effect";
import type { MachineId } from "#/db/tables";
import { createObserveCrashReceivedEvent } from "#/modules/inngest/events";
import type { CrashPushInput } from "#/modules/observe-ingest/ingest";
import { crashEvent, observeIngestCursor } from "#/modules/observe-ingest/tables";
import { enqueueEvent } from "#/modules/operations/outbox.server";
import { Database } from "#/server/database.server";
import { Unauthorized } from "#/server/public-error";

export type ObservePusher = { readonly organizationId: string; readonly machineId: MachineId };

export type AuthenticateObservePush<R = never> = (
  request: Request,
  machineId: string,
) => Effect.Effect<ObservePusher, Unauthorized, R>;

/** How a Machine proves it may push is not settled yet, so every push is refused until it is. */
export const authenticateObservePush: AuthenticateObservePush = () => Effect.fail(new Unauthorized());

export const ingestCrashPush = Effect.fn("ObserveIngest.crash")(function* (pusher: ObservePusher, push: CrashPushInput) {
  const database = yield* Database;
  yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const advanced = yield* drizzle.insert(observeIngestCursor)
      .values({ organizationId: pusher.organizationId, machineId: pusher.machineId, stream: "crash", seq: push.seq })
      .onConflictDoUpdate({
        target: [observeIngestCursor.organizationId, observeIngestCursor.machineId, observeIngestCursor.stream],
        set: { seq: sql`excluded.seq`, updatedAt: new Date() },
        setWhere: sql`${observeIngestCursor.seq} < excluded.seq`,
      })
      .returning({ seq: observeIngestCursor.seq });
    if (advanced.length === 0 || push.events.length === 0) return;
    const kept = yield* drizzle.insert(crashEvent)
      .values(push.events.map((event) => ({ ...event, organizationId: pusher.organizationId, machineId: pusher.machineId })))
      .onConflictDoNothing()
      .returning({ id: crashEvent.id, serviceId: crashEvent.serviceId });
    const firstByService = new Map<string, string>();
    for (const row of kept) if (!firstByService.has(row.serviceId)) firstByService.set(row.serviceId, row.id);
    for (const [serviceId, crashEventId] of firstByService) {
      yield* enqueueEvent(createObserveCrashReceivedEvent({ organizationId: pusher.organizationId, serviceId }, crashEventId));
    }
  }));
});
