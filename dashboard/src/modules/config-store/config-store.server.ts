import "@tanstack/react-start/server-only";
import { createRequire } from "node:module";
import type * as PloyzSdk from "@ployz/sdk";
import type { ConfigCommand, ConfigQuery, ConfigStore } from "@ployz/sdk";
import { Effect, Option, Schema } from "effect";
import { GitCommand, gatherGitEvidence } from "#/modules/config-store/git-evidence.server";
import { resolveCaller } from "#/modules/identity/caller.server";
import { AppConfig } from "#/server/config.server";
import { NotFound, Validation } from "#/server/public-error";

// SAFETY: the package exports this named CommonJS SDK surface at runtime.
const { openConfigStore, RpcError } = createRequire(import.meta.url)("@ployz/sdk") as Pick<
  typeof PloyzSdk,
  "openConfigStore" | "RpcError"
>;

/** One Store handle per database for the process; a failed open is retried by the next call. */
const opened = new Map<string, Promise<ConfigStore>>();

function storeAt(url: string) {
  let store = opened.get(url);
  if (store === undefined) {
    store = openConfigStore(url);
    opened.set(url, store);
    store.catch(() => opened.delete(url));
  }
  return store;
}

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

function refusal(error: { readonly code: string; readonly message: string; readonly details: unknown }) {
  const { code, message, details } = error;
  return Response.json({ error: { code, message, details } }, {
    status: statusFor(code),
    headers: { "cache-control": "no-store" },
  });
}

/**
 * `POST /api/config/read` and `/api/config/write`: the Config Store's two public operations, as the calling
 * Organization. Nothing else of the Store is reachable over HTTPS.
 */
export const handleConfigRequest = Effect.fn("ConfigStore.handle")(function* (request: Request) {
  const config = yield* AppConfig;
  // TODO(#1275): dark until the Config Store cutover, and only against its own database.
  const url = config.configStore.url;
  if (config.nodeEnv === "production" || url === undefined) return yield* new NotFound({ message: "Not found." });
  const operation = new URL(request.url).pathname.replace(/^\/api\/config\//, "");
  if (request.method !== "POST" || (operation !== "read" && operation !== "write")) {
    return yield* new NotFound({ message: "Not found." });
  }
  const caller = yield* resolveCaller(request.headers);
  const input: unknown = yield* Effect.tryPromise({
    try: () => request.json(),
    catch: () => new Validation({ message: "Expected a JSON body.", userFacing: true }),
  });
  const organization = caller.organization.id;
  const read = (query: ConfigQuery) => storeAt(url.href).then((store) => store.read(organization, query));
  const trusted = operation === "write"
    ? yield* gatherGitEvidence(organization, Option.getOrUndefined(Schema.decodeUnknownOption(GitCommand)(input)), read).pipe(
      Effect.catchTag("GithubObservationError", () => Effect.succeed(null)),
    )
    : undefined;
  if (trusted === null) return refusal({ code: "unavailable", message: "GitHub didn't answer; retry.", details: null });
  return yield* Effect.tryPromise({
    try: async () => {
      const store = await storeAt(url.href);
      // SAFETY: the Store decodes and validates the body itself, refusing anything else as invalid_argument.
      return operation === "read"
        ? await store.read(organization, input as ConfigQuery)
        : await store.write(organization, input as ConfigCommand, trusted);
    },
    catch: (cause) => cause,
  }).pipe(
    Effect.map((result) => Response.json(result, { headers: { "cache-control": "no-store" } })),
    Effect.catch((cause) => (cause instanceof RpcError ? Effect.succeed(refusal(cause)) : Effect.die(cause))),
  );
});
