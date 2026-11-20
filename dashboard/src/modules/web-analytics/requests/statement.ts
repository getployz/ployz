import { Effect, Schema } from "effect";
import { queryObserveOrg } from "#/modules/observe-store/clickhouse.server";

/**
 * The one place a request statement names its table. Every read of request data selects from `scoped`, which holds
 * only the Organization's rows (`org` is bound by `queryObserveOrg` from the session) in the Environment's namespaces.
 */
const SCOPED = "scoped AS (SELECT * FROM otel.ployz_request WHERE OrgId = {org:String} AND Namespace IN {namespaces:Array(String)})";

/** Counts and sums come back as numbers; the bot hashes travel as strings because a 64-bit hash outruns a JS number. */
const SETTINGS = "SETTINGS output_format_json_quote_64bit_integers = 0, max_memory_usage = 6000000000";

const ESCAPES = new Map([
  ["\\", "\\\\"], ["'", "\\'"], ["\n", "\\n"], ["\t", "\\t"], ["\r", "\\r"], ["\0", "\\0"], ["\b", "\\b"], ["\f", "\\f"],
]);
const escapeText = (value: string) => value.replace(/[\\'\n\t\r\0\b\f]/g, (char) => ESCAPES.get(char) ?? char);

/**
 * A statement's values. Each `bind` names its value `{pN:Type}` for the SQL to hold, so no statement text is built
 * from a value. `org` and `namespaces` belong to the scoped source.
 */
export class Params {
  readonly #values = new Map<string, string>();

  #bind(type: string, text: string) {
    const name = `p${this.#values.size}`;
    this.#values.set(name, text);
    return `{${name}:${type}}`;
  }

  string(value: string) { return this.#bind("String", escapeText(value)); }
  int(value: number) { return this.#bind("Int64", String(Math.trunc(value))); }
  strings(value: ReadonlyArray<string>) { return this.#bind("Array(String)", `[${value.map((item) => `'${escapeText(item)}'`).join(",")}]`); }
  /** Decimal digits of 64-bit hashes. */
  hashes(value: ReadonlyArray<string>) { return this.#bind("Array(UInt64)", `[${value.join(",")}]`); }
  /** An instant as the DateTime64(3, UTC) it is to ClickHouse. */
  instant(value: Date) { return `fromUnixTimestamp64Milli(${this.int(value.getTime())}, 'UTC')`; }

  get values(): ReadonlyMap<string, string> { return this.#values; }
}

export type Scope = { readonly organizationId: string; readonly namespaces: ReadonlyArray<string> };

/**
 * Runs `select` over `scoped` and the extra `ctes` (each `name AS (...)`, reading `scoped` or an earlier one). An
 * Environment with no namespace has no rows, so it reads none.
 */
export const readRequests = Effect.fn("WebAnalytics.read")(function* <Row>(
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
  const text = `WITH ${[SCOPED, ...ctes].join(",\n")}\n${select}\n${SETTINGS}`;
  if (/\botel\./.test([...ctes, select].join(" "))) return yield* Effect.die(new Error("A request read selects from `scoped`, never from an otel table."));
  const values = new Map(params.values);
  values.set("namespaces", `[${scope.namespaces.map((namespace) => `'${escapeText(namespace)}'`).join(",")}]`);
  return yield* queryObserveOrg(scope.organizationId, text, values, row);
});
