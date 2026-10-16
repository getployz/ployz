import { type ClickHouseClient, createClient } from "@clickhouse/client";
import { Context, Data, Effect, Layer, Redacted } from "effect";
import { AppConfig } from "#/server/config.server";

export class ClickHouseFailure extends Data.TaggedError("ClickHouseFailure")<{ readonly message: string; readonly cause: unknown }> {}

export type Params = Readonly<Record<string, string | number | boolean | ReadonlyArray<string> | ReadonlyArray<number>>>;

export type Unchecked = {
  readonly select: <A>(query: string, params?: Params) => Effect.Effect<ReadonlyArray<A>, ClickHouseFailure>;
  readonly command: (query: string, params?: Params) => Effect.Effect<void, ClickHouseFailure>;
  readonly insert: (table: string, rows: ReadonlyArray<object>) => Effect.Effect<void, ClickHouseFailure>;
};

export type ClickHouseService = {
  readonly unchecked: Unchecked;
  /** Seconds after a fence commits by which every write that passed its fence check has landed or been killed by max_execution_time. */
  readonly writeLifetimeSeconds: number;
};

export const ClickHouse = Context.Reference<ClickHouseService | null>("ployz/ClickHouse", { defaultValue: () => null });

const attempt = <A>(message: string, run: () => Promise<A>) =>
  Effect.tryPromise({ try: run, catch: (cause) => new ClickHouseFailure({ message, cause }) });

const STATEMENT_DEADLINE_SECONDS = 25;
/** The server deadline, plus the same again for a statement to travel from its fence check to the server. */
export const WRITE_LIFETIME_SECONDS = STATEMENT_DEADLINE_SECONDS * 2;

export const makeClickHouse = (client: ClickHouseClient, writeLifetimeSeconds = WRITE_LIFETIME_SECONDS): ClickHouseService => ({
  writeLifetimeSeconds,
  unchecked: {
    select: <A>(query: string, params?: Params) =>
      attempt("ClickHouse query failed.", async () =>
        (await client.query({ query, query_params: params, format: "JSONEachRow" })).json<A>()),
    command: (query, params) =>
      attempt("ClickHouse command failed.", () => client.command({ query, query_params: params })).pipe(Effect.asVoid),
    insert: (table, rows) =>
      rows.length === 0 ? Effect.void : attempt("ClickHouse insert failed.", () => client.insert({ table, values: rows, format: "JSONEachRow" })).pipe(Effect.asVoid),
  },
});

export const clickHouseClient = (url: string) =>
  Effect.acquireRelease(
    Effect.sync(() => createClient({
      url,
      request_timeout: (STATEMENT_DEADLINE_SECONDS + 5) * 1000,
      clickhouse_settings: {
        max_execution_time: STATEMENT_DEADLINE_SECONDS,
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
