import "@tanstack/react-start/server-only";
import { Effect, Redacted, Schema } from "effect";
import { AppConfig } from "#/server/config.server";
import { Unavailable } from "#/server/public-error";

/** Where Cloud's ClickHouse answers, and who it reads as. */
export type ClickhouseEndpoint = { readonly url: URL; readonly user: string; readonly password: Redacted.Redacted<string> };

/** One Service's logs, for one Organization. `organizationId` leads every query's WHERE. */
export type LogHistoryScope = {
  readonly organizationId: string;
  readonly namespace: string;
  readonly serviceId?: string | undefined;
  readonly serviceName?: string | undefined;
  readonly machineId?: string | undefined;
  /** Lines whose text holds this, ignoring case. */
  readonly search?: string | undefined;
};

export type LogHistoryRow = {
  /** Nanoseconds since the epoch, as a decimal string: it does not fit a JS number. */
  readonly timestamp: string;
  readonly machineId: string;
  readonly containerId: string;
  readonly serviceName: string;
  readonly stream: "stdout" | "stderr";
  readonly message: string;
  /** What orders lines that share a timestamp, so a cursor between them is exact. */
  readonly hash: string;
};

export type LogHistoryPage = { readonly rows: ReadonlyArray<LogHistoryRow>; readonly cursor: string | null };

/** The place after a page's last line: its timestamp and tie-break hash, both decimal digits. */
export const logHistoryCursor = Schema.String.check(Schema.isPattern(/^[0-9]{1,19}\.[0-9]{1,20}$/));

/**
 * A read looks back this far from where it starts and widens only when that holds fewer lines than the page, so a
 * busy Service's newest page never scans its whole retention.
 */
const WINDOWS_SECONDS = [3_600, 86_400, 7 * 86_400, 30 * 86_400] as const;
const QUERY_SECONDS = 20;
const NANOS = 1_000_000_000n;

const rowSchema = Schema.Struct({
  ts: Schema.String,
  hash: Schema.String,
  machine: Schema.String,
  container: Schema.String,
  service: Schema.String,
  stream: Schema.String,
  body: Schema.String,
});
const decodeRow = Schema.decodeUnknownSync(rowSchema);

const unavailable = (cause: unknown) => new Unavailable({ message: "Cloud's log history is not answering.", userFacing: true, cause });

/**
 * A read-only parameterized query: every value travels as a ClickHouse `param_` binding, never in the statement's text.
 * `readonly=2` lets the per-query limits below apply while still refusing writes and DDL.
 */
const run = Effect.fn("Runtime.clickhouseQuery")(function* (
  endpoint: ClickhouseEndpoint,
  statement: string,
  params: ReadonlyMap<string, string>,
) {
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
    catch: unavailable,
  });
  return text.trim() === "" ? [] : text.trim().split("\n").map((line) => decodeRow(JSON.parse(line)));
});

const statement = (scope: LogHistoryScope, cursor: boolean, from: boolean) => `
SELECT
  toString(toUnixTimestamp64Nano(Timestamp)) AS ts,
  toString(cityHash64(Body, LogAttributes['container.id'], ResourceAttributes['ployz.machine'])) AS hash,
  ResourceAttributes['ployz.machine'] AS machine,
  LogAttributes['container.id'] AS container,
  LogAttributes['ployz.service.name'] AS service,
  LogAttributes['log.iostream'] AS stream,
  Body AS body
FROM otel.otel_logs
WHERE OrgId = {org:String}
  AND LogAttributes['ployz.namespace'] = {namespace:String}
  ${scope.serviceId === undefined ? "" : "AND LogAttributes['ployz.service.id'] = {serviceId:String}"}
  ${scope.serviceId === undefined && scope.serviceName !== undefined ? "AND LogAttributes['ployz.service.name'] = {serviceName:String}" : ""}
  ${scope.machineId === undefined ? "" : "AND ResourceAttributes['ployz.machine'] = {machine:String}"}
  ${scope.search === undefined ? "" : "AND positionCaseInsensitiveUTF8(Body, {search:String}) > 0"}
  ${from ? "AND TimestampTime >= toDateTime({from:UInt32})" : ""}
  ${cursor ? `AND TimestampTime <= toDateTime(intDiv({cursorTs:Int64}, 1000000000))
  AND (Timestamp < fromUnixTimestamp64Nano({cursorTs:Int64})
    OR (Timestamp = fromUnixTimestamp64Nano({cursorTs:Int64}) AND cityHash64(Body, LogAttributes['container.id'], ResourceAttributes['ployz.machine']) < {cursorHash:UInt64}))` : ""}
ORDER BY Timestamp DESC, cityHash64(Body, LogAttributes['container.id'], ResourceAttributes['ployz.machine']) DESC
LIMIT {limit:UInt32}`;

/**
 * The newest `limit` lines of `scope` strictly older than `cursor` (or than now), newest first. The cursor is null
 * once nothing older is held.
 */
export const readLogHistory = Effect.fn("Runtime.readLogHistory")(function* (
  endpoint: ClickhouseEndpoint,
  scope: LogHistoryScope,
  options: { readonly limit: number; readonly cursor?: string | undefined; readonly now?: Date },
) {
  const [cursorTs, cursorHash] = options.cursor?.split(".") ?? [];
  const startSeconds = cursorTs === undefined
    ? Math.floor((options.now ?? new Date()).getTime() / 1000) + 1
    : Number(BigInt(cursorTs) / NANOS) + 1;
  const params = new Map<string, string>([["org", scope.organizationId], ["namespace", scope.namespace], ["limit", String(options.limit)]]);
  if (scope.serviceId !== undefined) params.set("serviceId", scope.serviceId);
  else if (scope.serviceName !== undefined) params.set("serviceName", scope.serviceName);
  if (scope.machineId !== undefined) params.set("machine", scope.machineId);
  if (scope.search !== undefined) params.set("search", scope.search);
  if (cursorTs !== undefined && cursorHash !== undefined) {
    params.set("cursorTs", cursorTs);
    params.set("cursorHash", cursorHash);
  }
  for (const [index, window] of WINDOWS_SECONDS.entries()) {
    const last = index === WINDOWS_SECONDS.length - 1;
    const rows = yield* run(
      endpoint,
      statement(scope, cursorTs !== undefined, !last),
      last ? params : new Map([...params, ["from", String(Math.max(0, startSeconds - window))]]),
    );
    if (rows.length < options.limit && !last) continue;
    const page = rows.map((row): LogHistoryRow => ({
      timestamp: row.ts, machineId: row.machine, containerId: row.container, serviceName: row.service,
      stream: row.stream === "stderr" ? "stderr" : "stdout", message: row.body, hash: row.hash,
    }));
    const oldest = page.at(-1);
    return { rows: page, cursor: oldest === undefined || page.length < options.limit ? null : `${oldest.timestamp}.${oldest.hash}` } satisfies LogHistoryPage;
  }
  return { rows: [], cursor: null } satisfies LogHistoryPage;
});

/** ClickHouse as this Cloud is configured to reach it, or undefined where it keeps no log history. */
export const clickhouseEndpoint = Effect.gen(function* () {
  const { observeClickhouse } = yield* AppConfig;
  return observeClickhouse;
});
