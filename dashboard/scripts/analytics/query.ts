// Asks the analytics store one question the way the dashboard would and prints the answer as JSON. Run from dashboard/:
//   pnpm analytics:query overview --site <environmentId> --range 30d
//   pnpm analytics:query breakdown --site <environmentId> --range 2026-10-01T00:00:00Z/2026-10-08T00:00:00Z --dim page --limit 20
// <query> is overview, timeseries (--interval hour|day), breakdown (--dim), sessions (--dim), entryPages or goalConversions.
// --range is Nd (the last N site-local days through the current hour) or <iso>/<iso> on site-local hour starts.
// --filter key:eq:value and key:neq:value repeat. The site's timezone comes from analytics.site; DATABASE_URL from .env.
import { parseArgs } from "node:util";
import { eq } from "drizzle-orm";
import { Effect, Layer } from "effect";
import * as analytics from "#/modules/web-analytics/adapter";
import { addDays, dayStart, HOUR_MS, hourStart, localDay } from "#/modules/web-analytics/local-time";
import { analyticsSite } from "#/modules/web-analytics/tables";
import { AppConfig } from "#/server/config.server";
import { Database, DatabaseLive } from "#/server/database.server";

const QUERIES = ["overview", "timeseries", "breakdown", "sessions", "entryPages", "goalConversions"] as const;
type QueryName = (typeof QUERIES)[number];

const oneOf = <T extends string>(allowed: ReadonlyArray<T>, value: string | undefined, what: string): T => {
  if (value !== undefined && (allowed as ReadonlyArray<string>).includes(value)) return value as T;
  throw new Error(`${what} must be one of ${allowed.join(", ")}, got ${value ?? "nothing"}.`);
};

const parseFilter = (text: string): analytics.Filter => {
  const match = /^([^:]+):(eq|neq):(.*)$/.exec(text);
  if (match === null) throw new Error(`--filter must be key:eq:value or key:neq:value, got ${text}.`);
  return { key: oneOf(analytics.FILTER_KEYS, match[1], "filter key"), op: match[2] as "eq" | "neq", value: match[3] ?? "" };
};

const parseRange = (text: string, timezone: string, now: Date): analytics.Range => {
  const days = /^(\d+)d$/.exec(text);
  if (days !== null) {
    const end = new Date(hourStart(timezone, now).getTime() + HOUR_MS);
    return { start: dayStart(timezone, addDays(timezone, localDay(timezone, end.getTime() - 1), 1 - Number(days[1]))), end };
  }
  const [start, end, ...rest] = text.split("/");
  if (start === undefined || end === undefined || rest.length > 0) throw new Error(`--range must be Nd or <iso>/<iso>, got ${text}.`);
  const range = { start: new Date(start), end: new Date(end) };
  if (Number.isNaN(range.start.getTime()) || Number.isNaN(range.end.getTime())) throw new Error(`--range bounds must be ISO instants, got ${text}.`);
  return range;
};

const siteOf = Effect.fn("analyticsQuery.site")(function* (environmentId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ timezone: analyticsSite.timezone }).from(analyticsSite).where(eq(analyticsSite.environmentId, environmentId));
  if (row === undefined) throw new Error(`No analytics.site row for environment ${environmentId}.`);
  const site: analytics.Site = { environmentId, timezone: row.timezone, retentionMonths: analytics.BASE_RETENTION_MONTHS };
  return site;
});

const ask = (name: QueryName, q: analytics.Query, options: { readonly interval?: string; readonly dim?: string }) => {
  switch (name) {
    case "overview":
      return analytics.overview(q);
    case "timeseries":
      return analytics.timeseries(q, oneOf(["hour", "day"], options.interval ?? "day", "--interval"));
    case "breakdown":
      return analytics.breakdown(q, oneOf(analytics.FILTER_KEYS, options.dim, "--dim"));
    case "sessions":
      return analytics.sessions(q, oneOf(analytics.SESSION_DIMS, options.dim, "--dim"));
    case "entryPages":
      return analytics.entryPages(q);
    case "goalConversions":
      return analytics.goalConversions(q);
  }
};

export async function run() {
  const { values, positionals } = parseArgs({
    args: process.argv.slice(3),
    allowPositionals: true,
    options: {
      site: { type: "string" },
      range: { type: "string", default: "30d" },
      filter: { type: "string", multiple: true, default: [] },
      limit: { type: "string", default: "10" },
      interval: { type: "string" },
      dim: { type: "string" },
    },
  });
  const name = oneOf(QUERIES, positionals[0], "<query>");
  if (values.site === undefined) throw new Error("--site <environmentId> is required.");
  const limit = Number(values.limit);
  if (!Number.isInteger(limit) || limit < 1) throw new Error(`--limit must be a positive integer, got ${values.limit}.`);
  const environmentId = values.site;
  const range = values.range;
  const filters = values.filter.map(parseFilter);

  const program = Effect.gen(function* () {
    const site = yield* siteOf(environmentId);
    const q: analytics.Query = { site, range: parseRange(range, site.timezone, new Date()), filters, limit };
    return yield* ask(name, q, values);
  });
  const answer = await Effect.runPromise(
    Effect.scoped(program.pipe(Effect.provide(DatabaseLive.pipe(Layer.provide(AppConfig.layer))))),
  );
  console.log(JSON.stringify(answer, null, 2));
}
