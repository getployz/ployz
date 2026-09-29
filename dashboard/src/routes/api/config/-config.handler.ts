import "@tanstack/react-start/server-only";
import { Effect, Option, Schema } from "effect";
import { callStore, refusal } from "#/modules/config-store/config-store.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { StoreCommand, StoreQuery, type StoreCall } from "#/modules/config-store/store.contract";
import { receiveUpload } from "#/modules/config-store/upload.server";
import { resolveCaller } from "#/modules/identity/caller.server";
import { NotFound, Validation } from "#/server/public-error";

/**
 * `POST /api/config/read` and `/api/config/write`: the Config Store's two public operations, as the calling
 * Organization. `POST /api/config/upload/<Deployment ID>` keeps a gzipped tar of the source a Deployment the CLI
 * admits next builds from. Nothing else of the Store is reachable over HTTPS.
 */
export const handleConfigRequest = Effect.fn("ConfigStore.handle")(function* (request: Request) {
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
  const query = operation === "read" ? Option.getOrUndefined(Schema.decodeUnknownOption(StoreQuery)(input)) : undefined;
  const command = operation === "write" ? Option.getOrUndefined(Schema.decodeUnknownOption(StoreCommand)(input)) : undefined;
  const call: StoreCall | undefined = query !== undefined ? { operation: "read", query }
    : command !== undefined ? { operation: "write", command } : undefined;
  if (call === undefined) {
    return refusal({ code: "invalid_argument", message: `Expected a ${operation === "read" ? "query" : "command"}.`, details: null });
  }
  // Only Cloud's own Organization removal forgets an Organization's configuration.
  if (call.operation === "write" && call.command.command === "remove_organization") {
    return refusal({ code: "unsupported", message: "Remove an Organization with `ployz org rm`.", details: null });
  }
  const result = yield* callStore(caller.organization.id, caller.userId, call);
  return result.ok ? Response.json(result.value, { headers: { "cache-control": "no-store" } }) : refusal(result.refusal);
});

