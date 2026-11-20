import { Effect, Schema } from "effect";
import { queryObserveOrg } from "#/modules/observe-store/clickhouse.server";

type Table = "ployz_request" | "ployz_pageview";

/**
 * The one place a request statement names its table. Every read of request data selects from `scoped`, which holds
 * only the Organization's rows (`org` is bound by `queryObserveOrg` from the session) in the Environment's namespaces.
 */
const scopedOf = (table: Table, namespaces: string) => `scoped AS (SELECT * FROM otel.${table} WHERE OrgId = {org:String} AND Namespace IN ${namespaces})`;

/** Counts and sums come back as numbers; the bot hashes travel as strings because a 64-bit hash outruns a JS number. */
const SETTINGS = "SETTINGS output_format_json_quote_64bit_integers = 0, max_memory_usage = 6000000000";

/**
 * A statement's values. Each `bind` names its value `{pN:Type}` for the SQL to hold, so no statement text is built
 * from a value. `org` belongs to the scoped source, which binds the Environment's namespaces itself.
 */
export class Params {
  readonly #values = new Map<string, string>();

  #bind(type: string, text: string) {
    const name = `p${this.#values.size}`;
    this.#values.set(name, text);
    return `{${name}:${type}}`;
  }

  /** The shared reader escapes each value once for ClickHouse's `param_` text, so a value goes in as it is. */
  string(value: string) { return this.#bind("String", value); }
  int(value: number) { return this.#bind("Int64", String(Math.trunc(value))); }
  /** An array of one `String` binding per item. ClickHouse reads an `Array(String)` param without unescaping it, so the reader's escaping would corrupt a value inside one. */
  strings(value: ReadonlyArray<string>) { return `[${value.map((item) => this.string(item)).join(", ")}]`; }
  /** Decimal digits of 64-bit hashes. */
  hashes(value: ReadonlyArray<string>) { return this.#bind("Array(UInt64)", `[${value.join(",")}]`); }
  instant(value: Date) { return `fromUnixTimestamp64Milli(${this.int(value.getTime())}, 'UTC')`; }

  get values(): ReadonlyMap<string, string> { return this.#values; }
}

export type Scope = { readonly organizationId: string; readonly namespaces: ReadonlyArray<string> };

const readScoped = Effect.fn("WebAnalytics.read")(function* <Row>(
  table: Table,
  scope: Scope,
  params: Params,
  ctes: ReadonlyArray<string>,
  select: string,
  row: Schema.Decoder<Row>,
) {
  if (scope.namespaces.length === 0) {
    const none: Array<Row> = [];
    return none;
  }
  const text = `WITH ${[scopedOf(table, params.strings(scope.namespaces)), ...ctes].join(",\n")}\n${select}\n${SETTINGS}`;
  if (/\botel\./.test([...ctes, select].join(" "))) return yield* Effect.die(new Error("A request read selects from `scoped`, never from an otel table."));
  return yield* queryObserveOrg(scope.organizationId, text, params.values, row);
});

/**
 * The extra `ctes` are each `name AS (...)`, reading `scoped` or an earlier one. An Environment with no namespace has
 * no rows, so it reads none.
 */
export const readRequests = <Row>(scope: Scope, params: Params, ctes: ReadonlyArray<string>, select: string, row: Schema.Decoder<Row>) =>
  readScoped("ployz_request", scope, params, ctes, select, row);

export const readPageviews = <Row>(scope: Scope, params: Params, ctes: ReadonlyArray<string>, select: string, row: Schema.Decoder<Row>) =>
  readScoped("ployz_pageview", scope, params, ctes, select, row);
