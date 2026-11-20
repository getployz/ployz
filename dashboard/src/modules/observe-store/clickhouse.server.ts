import "@tanstack/react-start/server-only";
import { Effect, Redacted, Schema } from "effect";
import { AppConfig } from "#/server/config.server";
import { Unavailable } from "#/server/public-error";

export type ClickhouseEndpoint = { readonly url: URL; readonly user: string; readonly password: Redacted.Redacted<string> };

export const OBSERVE_NOT_CONFIGURED = "This Cloud keeps no observe data.";

const QUERY_SECONDS = 20;
const ORG_FILTER = /\bWHERE\s+OrgId\s*=\s*\{org:String\}/;

export const clickhouseEndpoint = Effect.gen(function* () {
  const { observeClickhouse } = yield* AppConfig;
  return observeClickhouse;
});

export const observeConfigured = Effect.map(clickhouseEndpoint, (endpoint) => endpoint !== undefined);

/**
 * Runs `statement` against `endpoint` as a read-only parameterized query: every value travels as a ClickHouse
 * `param_` binding, never in the statement's text, and `readonly=2` lets the per-query limits apply while still
 * refusing writes and DDL. Returns each `JSONEachRow` line decoded by `row`.
 */
const queryClickhouse = Effect.fn("ObserveStore.query")(function* <Row>(
  endpoint: ClickhouseEndpoint,
  statement: string,
  params: ReadonlyMap<string, string>,
  row: Schema.Decoder<Row>,
) {
  const decodeRow = Schema.decodeUnknownSync(row);
  const target = new URL("/", endpoint.url);
  target.searchParams.set("default_format", "JSONEachRow");
  target.searchParams.set("readonly", "2");
  target.searchParams.set("max_execution_time", String(QUERY_SECONDS));
  for (const [name, value] of params) target.searchParams.set(`param_${name}`, value);
  const text = yield* Effect.tryPromise({
    try: async (signal) => {
      const response = await fetch(target, {
        method: "POST",
        headers: { authorization: `Basic ${Buffer.from(`${endpoint.user}:${Redacted.value(endpoint.password)}`).toString("base64")}` },
        body: statement,
        signal: AbortSignal.any([signal, AbortSignal.timeout((QUERY_SECONDS + 5) * 1000)]),
      });
      const body = await response.text();
      if (!response.ok) throw new Error(`ClickHouse answered ${response.status}: ${body.slice(0, 300)}`);
      return body;
    },
    catch: (cause) => new Unavailable({ message: "Cloud's observe data is not answering.", userFacing: true, cause }),
  });
  return text.trim() === "" ? [] : text.trim().split("\n").map((line): Row => decodeRow(JSON.parse(line)));
});

/**
 * The one way a request reads an Organization's observe data. `organizationId` comes from the session or the
 * verified CLI credential, never from the request. The helper binds it as `{org:String}`, refuses a statement that
 * does not filter `WHERE OrgId = {org:String}`, and refuses params that name `org`, so no caller can read another
 * Organization's rows or widen the filter.
 */
export const queryObserveOrgAt = Effect.fn("ObserveStore.queryOrg")(function* <Row>(
  endpoint: ClickhouseEndpoint,
  organizationId: string,
  statement: string,
  params: ReadonlyMap<string, string>,
  row: Schema.Decoder<Row>,
) {
  if (!ORG_FILTER.test(statement)) return yield* Effect.die(new Error("An observe query must filter WHERE OrgId = {org:String}."));
  if (params.has("org")) return yield* Effect.die(new Error("An observe query binds `org` itself."));
  return yield* queryClickhouse(endpoint, statement, new Map([["org", organizationId], ...params]), row);
});

/** `queryObserveOrgAt` against this Cloud's ClickHouse, or `Unavailable` (`OBSERVE_NOT_CONFIGURED`) where it has none. */
export const queryObserveOrg = Effect.fn("ObserveStore.queryOrgConfigured")(function* <Row>(
  organizationId: string,
  statement: string,
  params: ReadonlyMap<string, string>,
  row: Schema.Decoder<Row>,
) {
  const endpoint = yield* clickhouseEndpoint;
  if (endpoint === undefined) return yield* new Unavailable({ message: OBSERVE_NOT_CONFIGURED, userFacing: true });
  return yield* queryObserveOrgAt(endpoint, organizationId, statement, params, row);
});
