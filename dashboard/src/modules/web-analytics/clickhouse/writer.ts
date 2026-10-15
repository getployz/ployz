import { Effect } from "effect";
import { Database } from "#/server/database.server";
import { analyticsOutbox, type analyticsPageview } from "../tables";
import { RETENTION_DAYS } from "./schema";

type Pageview = typeof analyticsPageview.$inferSelect;

/** A `pageview` row as ClickHouse's JSONEachRow reads it: UTC wall time, and the visitor id quoted so it survives JSON. */
export const clickHouseRow = (row: Pageview) => ({
  environment_id: row.environmentId,
  ts: row.ts.toISOString().replace("T", " ").replace("Z", ""),
  visitor_id: row.visitorId.toString(),
  host: row.host,
  pathname: row.pathname,
  source: row.source,
  referrer_host: row.referrerHost,
  utm_source: row.utmSource,
  utm_medium: row.utmMedium,
  utm_campaign: row.utmCampaign,
  utm_term: row.utmTerm,
  utm_content: row.utmContent,
  ref: row.ref,
  language: row.language,
  flags: row.flags,
  retention_days: RETENTION_DAYS,
});

/** Runs inside ingest's transaction, so a batch reaches the outbox exactly when its rows reach Postgres. */
export const enqueueForClickHouse = Effect.fn("WebAnalytics.enqueueForClickHouse")(function* (rows: ReadonlyArray<Pageview>) {
  const byEnvironment = new Map<string, Array<ReturnType<typeof clickHouseRow>>>();
  for (const row of rows) byEnvironment.set(row.environmentId, [...(byEnvironment.get(row.environmentId) ?? []), clickHouseRow(row)]);
  if (byEnvironment.size === 0) return;
  const { drizzle } = yield* Database;
  yield* drizzle.insert(analyticsOutbox).values(
    [...byEnvironment].map(([environmentId, batch]) => ({ environmentId, rows: batch })),
  );
});
