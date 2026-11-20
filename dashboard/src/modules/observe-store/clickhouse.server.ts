import "@tanstack/react-start/server-only";
import { createClient } from "@clickhouse/client";
import { Effect, Redacted } from "effect";
import { AppConfig } from "#/server/config.server";
import { Unavailable } from "#/server/public-error";

/** Cloud's observe tables, which hold every Organization's rows. */
const OBSERVE_TABLE = /\botel\.otel_[a-z_]+/g;
/** The only tenancy filter a query may use. `org` is bound by the helper, never by the caller. */
const ORG_FILTER = /\bOrgId\s*=\s*\{org:String\}/g;
const QUERY_SECONDS = 10;
const RESULT_ROWS = 200_000;

/** Values a query binds by name. Only scalars: ClickHouse substitutes them, never the text. */
export type QueryParams = Readonly<Record<string, string | number>>;

/** Throws unless every read of an observe table in `query` is filtered by the Organization. */
export function assertOrgScoped(query: string, params: QueryParams): void {
  if ("org" in params) throw new Error("A query binds the Organization itself; pass no `org` parameter.");
  const reads = query.match(OBSERVE_TABLE)?.length ?? 0;
  const filters = query.match(ORG_FILTER)?.length ?? 0;
  if (reads === 0 || filters < reads) {
    throw new Error("Every read of an observe table filters `OrgId = {org:String}`.");
  }
}

/** Reads rows for one Organization. The query holds `OrgId = {org:String}` beside each table it reads. */
export type OrgQuery = <Row>(query: string, params?: QueryParams) => Effect.Effect<ReadonlyArray<Row>, Unavailable>;

export type ObserveStore = { readonly type: "not_configured" } | { readonly type: "ready"; readonly query: OrgQuery };

/**
 * Opens Cloud's ClickHouse for the scope's life. `organizationId` comes from the session, so a caller never names
 * another Organization's rows. Without ClickHouse configured the result is `not_configured`.
 */
export const openObserveStore = Effect.fn("ObserveStore.open")(function* (organizationId: string) {
  const { observeClickhouse } = yield* AppConfig;
  if (observeClickhouse === undefined) return { type: "not_configured" } satisfies ObserveStore;
  const client = yield* Effect.acquireRelease(
    Effect.sync(() => createClient({
      url: observeClickhouse.url.href,
      username: observeClickhouse.username,
      password: observeClickhouse.password === undefined ? "" : Redacted.value(observeClickhouse.password),
      database: "otel",
      request_timeout: QUERY_SECONDS * 1000,
      max_open_connections: 4,
      clickhouse_settings: { max_execution_time: QUERY_SECONDS, max_result_rows: String(RESULT_ROWS), result_overflow_mode: "throw" },
    })),
    (opened) => Effect.promise(() => opened.close()),
  );
  const query: OrgQuery = <Row>(text: string, params: QueryParams = {}) => Effect.suspend(() => {
    assertOrgScoped(text, params);
    return Effect.tryPromise({
      try: async () => {
        const result = await client.query({ query: text, query_params: { ...params, org: organizationId }, format: "JSONEachRow" });
        return await result.json<Row>();
      },
      catch: (cause) => new Unavailable({ message: "Cloud couldn’t read metrics. Try again.", userFacing: true, cause }),
    });
  });
  return { type: "ready", query } satisfies ObserveStore;
});
