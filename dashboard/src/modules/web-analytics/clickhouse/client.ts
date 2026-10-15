import { type ClickHouseClient, createClient } from "@clickhouse/client";
import { Context, Data, Effect, Layer, Redacted } from "effect";
import { AppConfig } from "#/server/config.server";

export class ClickHouseFailure extends Data.TaggedError("ClickHouseFailure")<{ readonly message: string; readonly cause: unknown }> {}

type Params = Readonly<Record<string, string | number | boolean | ReadonlyArray<string>>>;

export type ClickHouseService = {
  readonly query: <A>(query: string, params?: Params) => Effect.Effect<ReadonlyArray<A>, ClickHouseFailure>;
  readonly command: (query: string, params?: Params) => Effect.Effect<void, ClickHouseFailure>;
  /** A retry with the same `token` is dropped by the table's deduplication window, so a replayed batch lands once. */
  readonly insert: (table: string, rows: ReadonlyArray<object>, token?: string) => Effect.Effect<void, ClickHouseFailure>;
};

/** Present only where `CLICKHOUSE_URL` is set; Self-hosted Cloud and the Postgres tests never have one. */
export const ClickHouse = Context.Reference<ClickHouseService | null>("ployz/ClickHouse", { defaultValue: () => null });

const attempt = <A>(message: string, run: () => Promise<A>) =>
  Effect.tryPromise({ try: run, catch: (cause) => new ClickHouseFailure({ message, cause }) });

export const makeClickHouse = (client: ClickHouseClient): ClickHouseService => ({
  query: <A>(query: string, params?: Params) =>
    attempt("ClickHouse query failed.", async () =>
      (await client.query({ query, query_params: params, format: "JSONEachRow" })).json<A>()),
  command: (query, params) =>
    attempt("ClickHouse command failed.", () => client.command({ query, query_params: params })).pipe(Effect.asVoid),
  insert: (table, rows, token) =>
    rows.length === 0 ? Effect.void : attempt("ClickHouse insert failed.", () => client.insert({
      table,
      values: rows,
      format: "JSONEachRow",
      clickhouse_settings: token === undefined ? undefined : { insert_deduplicate: 1, insert_deduplication_token: token },
    })).pipe(Effect.asVoid),
});

export const clickHouseClient = (url: string) =>
  Effect.acquireRelease(
    Effect.sync(() => createClient({
      url,
      clickhouse_settings: {
        output_format_json_quote_64bit_integers: 0,
        date_time_output_format: "iso",
        // Lightweight deletes and the inserts after them must be visible to the next read of the same rollup run.
        mutations_sync: "2",
      },
    })),
    (client) => Effect.promise(() => client.close()),
  );

export const ClickHouseLive = Layer.effect(
  ClickHouse,
  Effect.gen(function* () {
    const { clickhouse } = yield* AppConfig;
    if (clickhouse === null) return null;
    return makeClickHouse(yield* clickHouseClient(Redacted.value(clickhouse.url)));
  }),
);
