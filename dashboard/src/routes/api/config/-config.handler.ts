import "@tanstack/react-start/server-only";
import type { ConfigCommand, ConfigQuery } from "@ployz/sdk";
import { Effect } from "effect";
import { callStore, cloudStore } from "#/modules/config-store/config-store.server";
import type { StoreCall, StoreRefusal } from "#/modules/config-store/store.contract";
import { receiveUpload } from "#/modules/config-store/upload.server";
import { resolveCaller } from "#/modules/identity/caller.server";
import { AppConfig } from "#/server/config.server";
import { NotFound, Validation } from "#/server/public-error";

/** A Store refusal travels to the CLI verbatim: the RPC error vocabulary, never the rejected value. */
function statusFor(code: string) {
  switch (code) {
    case "invalid_argument":
      return 422;
    case "not_found":
      return 404;
    case "conflict":
    case "ambiguous":
    case "confirmation_required":
      return 409;
    case "unauthenticated":
      return 401;
    case "unsupported":
      return 501;
    case "unavailable":
      return 503;
    default:
      return 500;
  }
}

function refusal(error: StoreRefusal) {
  const { code, message, details } = error;
  return Response.json({ error: { code, message, details } }, {
    status: statusFor(code),
    headers: { "cache-control": "no-store" },
  });
}

/**
 * `POST /api/config/read` and `/api/config/write`: the Config Store's two public operations, as the calling
 * Organization. `POST /api/config/upload/<Deployment ID>` keeps a gzipped tar of the source a Deployment the CLI
 * admits next builds from. Nothing else of the Store is reachable over HTTPS.
 */
export const handleConfigRequest = Effect.fn("ConfigStore.handle")(function* (request: Request) {
  const config = yield* AppConfig;
  // TODO(#1275): dark in production until the Config Store cutover.
  if (config.nodeEnv === "production") return yield* new NotFound({ message: "Not found." });
  const operation = new URL(request.url).pathname.replace(/^\/api\/config\//, "");
  const upload = /^upload\/([^/]+)$/.exec(operation)?.[1];
  if (request.method !== "POST" || (operation !== "read" && operation !== "write" && upload === undefined)) {
    return yield* new NotFound({ message: "Not found." });
  }
  // Telemetry only: the CLI names a detected coding agent; nothing else reads it.
  yield* Effect.annotateCurrentSpan("ployz.agent", request.headers.get("x-ployz-agent") ?? "none");
  const caller = yield* resolveCaller(request.headers);
  if (upload !== undefined) {
    const refused = yield* receiveUpload(yield* cloudStore, caller.organization.id, decodeURIComponent(upload), request.body);
    return refused === undefined
      ? Response.json({ uploaded: decodeURIComponent(upload) }, { headers: { "cache-control": "no-store" } })
      : refusal(refused);
  }
  const input: unknown = yield* Effect.tryPromise({
    try: () => request.json(),
    catch: () => new Validation({ message: "Expected a JSON body.", userFacing: true }),
  });
  // SAFETY: the Store decodes and validates the body itself, refusing anything else as invalid_argument.
  const call: StoreCall = operation === "read" ? { operation, query: input as ConfigQuery } : { operation: "write", command: input as ConfigCommand };
  const result = yield* callStore(caller.organization.id, caller.userId, call);
  return result.ok ? Response.json(result.value, { headers: { "cache-control": "no-store" } }) : refusal(result.refusal);
});

