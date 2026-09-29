import type { ConfigCommand, ConfigQuery, ConfigView, ConfigWritten, EnvironmentRef, JsonValue } from "@ployz/sdk";
import { Schema } from "effect";

// TODO(#1275): the Config Store is dark in production until the cutover; production keeps the old backend.
/** Whether this build reads and writes authored state through the Config Store. */
export const storeEnabled = !import.meta.env.PROD;

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

/** A Service in the Store: its Environment and its name there. */
export type StoreServiceRef = { environment: EnvironmentRef; service: string };

/** The Store decodes and validates queries and commands itself, refusing anything else as `invalid_argument`. */
export const storeReadInput = Schema.Struct({ organizationSlug: Schema.String, query: Schema.Json });
export const storeWriteInput = Schema.Struct({ organizationSlug: Schema.String, command: Schema.Json });

export type { ConfigCommand, ConfigQuery, ConfigView, ConfigWritten };
