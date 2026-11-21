import "@tanstack/react-start/server-only";
import { Effect, Schema } from "effect";
import { type ClickhouseEndpoint, queryObserveOrgAt } from "#/modules/observe-store/clickhouse.server";

export type LogHistoryScope = {
  readonly organizationId: string;
  readonly namespace: string;
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

const WINDOWS_SECONDS = [3_600, 86_400, 7 * 86_400, 30 * 86_400] as const;
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

/** Orders lines that share a timestamp. Every field that can tell two lines apart goes in, so a cursor between them is exact. */
const TIE_BREAK = "cityHash64(Body, LogAttributes['container.id'], ResourceAttributes['ployz.machine'], LogAttributes['log.iostream'])";

const statement = (scope: LogHistoryScope, cursor: boolean, from: boolean) => `
SELECT
  toString(toUnixTimestamp64Nano(Timestamp)) AS ts,
  toString(${TIE_BREAK}) AS hash,
  ResourceAttributes['ployz.machine'] AS machine,
  LogAttributes['container.id'] AS container,
  LogAttributes['ployz.service.name'] AS service,
  LogAttributes['log.iostream'] AS stream,
  Body AS body
FROM otel.otel_logs
WHERE OrgId = {org:String}
  AND LogAttributes['ployz.namespace'] = {namespace:String}
  ${scope.serviceName === undefined ? "" : "AND LogAttributes['ployz.service.name'] = {serviceName:String}"}
  ${scope.machineId === undefined ? "" : "AND ResourceAttributes['ployz.machine'] = {machine:String}"}
  ${scope.search === undefined ? "" : "AND positionCaseInsensitiveUTF8(Body, {search:String}) > 0"}
  ${from ? "AND TimestampTime >= toDateTime({from:UInt32})" : ""}
  ${cursor ? `AND TimestampTime <= toDateTime(intDiv({cursorTs:Int64}, 1000000000))
  AND (Timestamp < fromUnixTimestamp64Nano({cursorTs:Int64})
    OR (Timestamp = fromUnixTimestamp64Nano({cursorTs:Int64}) AND ${TIE_BREAK} < {cursorHash:UInt64}))` : ""}
ORDER BY Timestamp DESC, ${TIE_BREAK} DESC
LIMIT {limit:UInt32}`;

const MAX_SERVICES = 1000;
const serviceRow = Schema.Struct({ service: Schema.String });

/** The names of the Services of a namespace with lines in the last 30 days, removed ones included. */
export const readLogServices = Effect.fn("Runtime.readLogServices")(function* (
  endpoint: ClickhouseEndpoint,
  scope: Pick<LogHistoryScope, "organizationId" | "namespace">,
) {
  const rows = yield* queryObserveOrgAt(
    endpoint,
    scope.organizationId,
    `SELECT DISTINCT LogAttributes['ployz.service.name'] AS service
FROM otel.otel_logs
WHERE OrgId = {org:String}
  AND LogAttributes['ployz.namespace'] = {namespace:String}
  AND TimestampTime >= now() - toIntervalDay(30)
  AND service != ''
ORDER BY service
LIMIT {limit:UInt32}`,
    new Map([["namespace", scope.namespace], ["limit", String(MAX_SERVICES)]]),
    serviceRow,
  );
  return rows.map((row) => row.service);
});

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
  const params = new Map<string, string>([["namespace", scope.namespace], ["limit", String(options.limit)]]);
  if (scope.serviceName !== undefined) params.set("serviceName", scope.serviceName);
  if (scope.machineId !== undefined) params.set("machine", scope.machineId);
  if (scope.search !== undefined) params.set("search", scope.search);
  if (cursorTs !== undefined && cursorHash !== undefined) {
    params.set("cursorTs", cursorTs);
    params.set("cursorHash", cursorHash);
  }
  for (const [index, window] of WINDOWS_SECONDS.entries()) {
    const last = index === WINDOWS_SECONDS.length - 1;
    const rows = yield* queryObserveOrgAt(
      endpoint,
      scope.organizationId,
      statement(scope, cursorTs !== undefined, !last),
      last ? params : new Map([...params, ["from", String(Math.max(0, startSeconds - window))]]),
      rowSchema,
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
