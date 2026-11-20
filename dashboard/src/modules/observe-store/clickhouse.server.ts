import "@tanstack/react-start/server-only";
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
  const authorization = `Basic ${Buffer.from(`${observeClickhouse.username}:${observeClickhouse.password === undefined ? "" : Redacted.value(observeClickhouse.password)}`).toString("base64")}`;
  const query: OrgQuery = <Row>(text: string, params: QueryParams = {}) => Effect.suspend(() => {
    assertOrgScoped(text, params);
    // Every value travels as a `param_` binding, never in the statement's text. `readonly=2` refuses writes and DDL
    // but still lets the per-query limits below apply.
    const target = new URL("/", observeClickhouse.url);
    target.searchParams.set("default_format", "JSONEachRow");
    target.searchParams.set("database", "otel");
    target.searchParams.set("readonly", "2");
    target.searchParams.set("max_execution_time", String(QUERY_SECONDS));
    target.searchParams.set("max_result_rows", String(RESULT_ROWS));
    target.searchParams.set("result_overflow_mode", "throw");
    for (const [name, value] of Object.entries({ ...params, org: organizationId })) target.searchParams.set(`param_${name}`, String(value));
    return Effect.tryPromise({
      try: async (signal) => {
        const response = await fetch(target, {
          method: "POST",
          headers: { authorization },
          body: text,
          signal: AbortSignal.any([signal, AbortSignal.timeout((QUERY_SECONDS + 5) * 1000)]),
        });
        const body = await response.text();
        if (!response.ok) throw new Error(`ClickHouse answered ${response.status}: ${body.slice(0, 300)}`);
        const lines = body.trim();
        // SAFETY: the caller's query text fixes the row's columns; ClickHouse answers exactly those.
        return lines === "" ? [] : lines.split("\n").map((line) => JSON.parse(line) as Row);
      },
      catch: (cause) => new Unavailable({ message: "Cloud couldn’t read metrics. Try again.", userFacing: true, cause }),
    });
  });
  return { type: "ready", query } satisfies ObserveStore;
});
