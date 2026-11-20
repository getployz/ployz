import "@tanstack/react-start/server-only";
import { and, eq, isNull, lt, or, sql } from "drizzle-orm";
import { Clock, Effect, Option } from "effect";
import type { MachineId } from "#/db/tables";
import { machineTokenCurrent, parseMachineToken } from "#/modules/observe-ingest/machine-token";
import { organizationOfSigningMachine, verifyManagementSignature } from "#/modules/observe-ingest/machine-signers.server";
import { organizationMachine } from "#/modules/machines/tables";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { Unauthorized, Unavailable } from "#/server/public-error";

export const OTLP_SIGNALS = ["logs", "metrics", "traces"] as const;
export type OtlpSignal = (typeof OTLP_SIGNALS)[number];

export type OtlpSender = { readonly organizationId: string; readonly machineId: MachineId };

/** Days Cloud keeps a Machine's records. Constant for now: it is the one retention Cloud sells. */
const RETENTION_DAYS = "30";
const FORWARD_TIMEOUT_MS = 20_000;
/** OTLP exporters treat 400 as "do not retry this batch". */
const badBody = (message: string) => Response.json({ error: message }, { status: 400, headers: { "cache-control": "no-store" } });
const ENCODINGS = new Set(["gzip", "zstd", "identity"]);

/** The Machine that signed this token and the Organization it is enrolled in. Everything Cloud stamps on a record comes from here. */
export const authenticateMachineToken = Effect.fn("ObserveIngest.otlpToken")(function* (authorization: string | null) {
  const token = parseMachineToken(authorization);
  if (Option.isNone(token)) return yield* new Unauthorized();
  const nowSeconds = Math.floor((yield* Clock.currentTimeMillis) / 1000);
  if (!machineTokenCurrent(token.value, nowSeconds)) return yield* new Unauthorized();
  const { machine, message, signature } = token.value;
  const organizationId = yield* organizationOfSigningMachine(machine, (identity) => verifyManagementSignature(identity, message, signature));
  return { organizationId, machineId: machine };
});

const SEEN_GRANULARITY_SECONDS = 30;

export const recordMachineSeen = Effect.fn("ObserveIngest.machineSeen")(function* (sender: OtlpSender) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(organizationMachine).set({ observeSeenAt: sql`now()` }).where(and(
    eq(organizationMachine.organizationId, sender.organizationId),
    eq(organizationMachine.machineId, sender.machineId),
    or(
      isNull(organizationMachine.observeSeenAt),
      lt(organizationMachine.observeSeenAt, sql`now() - make_interval(secs => ${SEEN_GRANULARITY_SECONDS})`),
    ),
  ));
});

/**
 * Hands a verified Machine's OTLP body to Cloud's Collector. Only the three headers Cloud writes carry tenancy: nothing
 * the Machine sent is forwarded but the body's type and encoding.
 */
export const forwardOtlp = Effect.fn("ObserveIngest.otlpForward")(function* (
  sender: OtlpSender,
  signal: OtlpSignal,
  headers: Headers,
  body: Uint8Array,
) {
  const { observeCollectorUrl } = yield* AppConfig;
  if (observeCollectorUrl === undefined) {
    return yield* new Unavailable({ message: "This Cloud does not keep observe data.", userFacing: true });
  }
  const contentType = headers.get("content-type")?.split(";")[0]?.trim().toLowerCase();
  if (contentType !== "application/x-protobuf" && contentType !== "application/json") {
    return badBody("OTLP bodies are application/x-protobuf or application/json.");
  }
  const forward = new Headers({
    "content-type": contentType,
    "x-ployz-org": sender.organizationId,
    "x-ployz-machine": sender.machineId,
    "x-ployz-retention-days": RETENTION_DAYS,
  });
  const encoding = headers.get("content-encoding")?.trim().toLowerCase();
  if (encoding !== undefined) {
    if (!ENCODINGS.has(encoding)) return badBody("OTLP bodies are gzip, zstd or uncompressed.");
    if (encoding !== "identity") forward.set("content-encoding", encoding);
  }
  const upstream = yield* Effect.tryPromise({
    try: (signalAbort) => fetch(`${observeCollectorUrl.origin}/v1/${signal}`, {
      method: "POST",
      headers: forward,
      body: new Uint8Array(body),
      signal: AbortSignal.any([signalAbort, AbortSignal.timeout(FORWARD_TIMEOUT_MS)]),
    }),
    catch: (cause) => new Unavailable({ message: "Cloud's observe Collector is not answering.", userFacing: true, cause }),
  });
  if (upstream.ok) {
    const reply = yield* Effect.tryPromise({
      try: () => upstream.arrayBuffer(),
      catch: (cause) => new Unavailable({ message: "Cloud's observe Collector is not answering.", userFacing: true, cause }),
    });
    yield* recordMachineSeen(sender).pipe(Effect.catchCause((cause) => Effect.logWarning("Cannot record a Machine's last report.", { cause })));
    return new Response(reply, {
      status: 200,
      headers: { "content-type": upstream.headers.get("content-type") ?? contentType, "cache-control": "no-store" },
    });
  }
  // The Collector has no auth of its own, so a 4xx is the Collector refusing the body itself.
  if (upstream.status >= 400 && upstream.status < 500 && upstream.status !== 429) {
    return badBody("The Collector could not read that OTLP body.");
  }
  return yield* new Unavailable({ message: "Cloud's observe Collector is not keeping up.", userFacing: true });
});
