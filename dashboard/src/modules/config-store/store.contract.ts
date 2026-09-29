import type { ConfigCommand, ConfigQuery, ConfigView, ConfigWritten, JsonValue } from "@ployz/sdk";
import { Schema } from "effect";

/** A Store refusal as the Store words it: the RPC error vocabulary, never the rejected value. */
export type StoreRefusal = { code: string; message: string; details: JsonValue };

/**
 * What a Store read or write returns across the server-function boundary. A refusal is an outcome, not a failure:
 * `conflict` carries fresh state and `not_found` names what's missing, so callers need the code and details intact.
 */
export type StoreResult<T> = { ok: true; value: T } | { ok: false; refusal: StoreRefusal };

/** One Store operation: a read answers a query with a view; a write applies a command. */
export type StoreCall = { operation: "read"; query: ConfigQuery } | { operation: "write"; command: ConfigCommand };

/** The view a query answers with: each query kind has the view of the same name. */
export type StoreViewOf<Q extends ConfigQuery> = Extract<ConfigView, { view: Q["query"] }>;

/** Reads one view in the calling Organization. */
export type StoreRead = <Q extends ConfigQuery>(query: Q) => Promise<StoreViewOf<Q>>;

/** What a call answers with: a read its query's view, a write what it wrote. */
export type StoreAnswer<C extends StoreCall> = C extends { operation: "read"; query: infer Q extends ConfigQuery } ? StoreViewOf<Q> : ConfigWritten;

/**
 * A query or command from outside Cloud, checked once where it enters: an object naming its kind. The Store decodes
 * and validates the rest itself, refusing anything else as `invalid_argument`.
 */
const envelope = <T>(kind: "query" | "command") => {
  const named = Schema.is(Schema.Struct({ [kind]: Schema.String }));
  return Schema.declare<T>((value): value is T => named(value));
};
export const StoreQuery = envelope<ConfigQuery>("query");
export const StoreCommand = envelope<ConfigCommand>("command");

export const storeReadInput = Schema.Struct({ organizationSlug: Schema.String, query: StoreQuery });
export const storeWriteInput = Schema.Struct({ organizationSlug: Schema.String, command: StoreCommand });

/** An Environment as calls name it: either part left out means the default. */
export const EnvironmentRef = Schema.Struct({
  project: Schema.optional(Schema.NullOr(Schema.String)),
  environment: Schema.optional(Schema.NullOr(Schema.String)),
});
export const environmentOf = (ref: typeof EnvironmentRef.Type | undefined) =>
  ({ project: ref?.project ?? null, environment: ref?.environment ?? null });

export type { ConfigCommand, ConfigQuery, ConfigView, ConfigWritten };
