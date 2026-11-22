import "@tanstack/react-start/server-only";
import { gunzip } from "node:zlib";
import { promisify } from "node:util";
import { Effect } from "effect";
import { createObserveCrashReceivedEvent } from "#/modules/observe-ingest/observe.events";
import { type CrashEventInput, crashEventsOf } from "#/modules/observe-ingest/crash-logs";
import type { ObservePusher } from "#/modules/observe-ingest/ingest.server";
import { crashEvent } from "#/modules/observe-ingest/tables";
import { enqueueEvent } from "#/modules/inngest/outbox.server";
import { Database } from "#/server/database.server";
import { Unavailable } from "#/server/public-error";

const MAX_DECODED_BYTES = 8 * 1024 * 1024;
const gunzipCapped = promisify(gunzip);

/** The crash events in an OTLP logs body Cloud already forwarded. A body that is not JSON, or not readable, has none. */
export const crashEventsInBody = Effect.fn("ObserveIngest.crashEvents")(function* (headers: Headers, body: Uint8Array) {
  const contentType = headers.get("content-type")?.split(";")[0]?.trim().toLowerCase();
  const encoding = headers.get("content-encoding")?.trim().toLowerCase() ?? "identity";
  if (contentType !== "application/json" || (encoding !== "identity" && encoding !== "gzip")) return [];
  const text = yield* Effect.tryPromise(async () =>
    new TextDecoder().decode(encoding === "gzip" ? await gunzipCapped(body, { maxOutputLength: MAX_DECODED_BYTES }) : body));
  return crashEventsOf(text);
}, Effect.orElseSucceed((): ReadonlyArray<CrashEventInput> => []));

/**
 * Keeps each event once per Machine and asks for one mail per Service from the events it had not kept, so a Machine's
 * retry of a batch the ledger already holds mails nothing again.
 */
export const recordCrashEvents = Effect.fn("ObserveIngest.recordCrashEvents")(function* (
  pusher: ObservePusher,
  events: ReadonlyArray<CrashEventInput>,
) {
  if (events.length === 0) return;
  const database = yield* Database;
  yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const kept = yield* drizzle.insert(crashEvent)
      .values(events.map((event) => ({ ...event, organizationId: pusher.organizationId, machineId: pusher.machineId })))
      .onConflictDoNothing()
      .returning({ id: crashEvent.id, serviceId: crashEvent.serviceId });
    const firstByService = new Map<string, string>();
    for (const row of kept) if (!firstByService.has(row.serviceId)) firstByService.set(row.serviceId, row.id);
    for (const [serviceId, crashEventId] of firstByService) {
      yield* enqueueEvent(createObserveCrashReceivedEvent({ organizationId: pusher.organizationId, serviceId }, crashEventId));
    }
  }));
}, Effect.catchCause((cause) =>
  Effect.fail(new Unavailable({ message: "Cloud could not keep the crash events.", userFacing: true, cause }))));

export const recordCrashEventsOf = (pusher: ObservePusher, headers: Headers, body: Uint8Array) =>
  Effect.flatMap(crashEventsInBody(headers, body), (events) => recordCrashEvents(pusher, events));
