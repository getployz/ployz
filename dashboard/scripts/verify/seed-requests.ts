// Seeds request rows for the verify stack's `shop/production` straight into ClickHouse's `otel.ployz_request`, so Analytics
// shows Web, API and Errors. Turns analytics on for production in UTC, then inserts 30 days of requests.
// Needs OBSERVE_CLICKHOUSE_URL (and CLICKHOUSE_USER / CLICKHOUSE_PASSWORD) in the Cloud's env, and analytics on in Cloud:
// run up.sh with BILLING=1 (hosted), or set ANALYTICS_ENABLED=true.
// To run from dashboard/: node --env-file=.verify/run/env scripts/verify/runner.mjs /scripts/verify/seed-requests.ts
//   web   HTML pages with visitors, sources and campaigns, a few 404s, and Googlebot crawling
//   api   JSON routes with ids in their paths, a slow /reports/:id, and 5xx bursts on /checkout
// Rows land on fixed request ids, so a rerun converges once ClickHouse merges them. Timestamps are written in UTC, which
// is the zone of ClickHouse's stock image.
import { createHash } from "node:crypto";
import { sql } from "drizzle-orm";
import { Effect, Redacted } from "effect";
import { clickhouseEndpoint } from "#/modules/observe-store/clickhouse.server";
import { namespacesByEnvironment, setSiteAnalytics } from "#/modules/web-analytics/salts.server";
import { Database } from "#/server/database.server";
import { AppRuntime } from "#/server/runtime.server";

const HOUR = 3_600_000;
const DAYS = 30;

/** A repeatable stream of numbers in [0, 1), so every run seeds the same rows. */
function random(seed: number) {
  let state = seed;
  return () => {
    state = (state * 1_103_515_245 + 12_345) % 2_147_483_648;
    return state / 2_147_483_648;
  };
}

const PAGES = ["/", "/", "/", "/pricing", "/pricing", "/docs", "/docs/install", "/blog/launch", "/signup", "/signup/done"];
const SOURCES: ReadonlyArray<{ referrer: string | null; query: Utm }> = [
  { referrer: null, query: {} },
  { referrer: "www.google.com", query: {} },
  { referrer: "news.ycombinator.com", query: {} },
  { referrer: null, query: { utm_source: "newsletter", utm_campaign: "october" } },
  { referrer: "twitter.com", query: { utm_campaign: "launch" } },
];
const LANGUAGES = ["en-US", "en-GB", "de-DE", "fr-FR", "ja-JP"];
const ROUTES = [
  { method: "GET", path: (n: number) => `/users/${n}`, ms: 18, fail: 0.01 },
  { method: "GET", path: (n: number) => `/orders/${n}/items`, ms: 42, fail: 0.02 },
  { method: "POST", path: () => "/checkout", ms: 220, fail: 0.06 },
  { method: "GET", path: (n: number) => `/reports/${n}`, ms: 1400, fail: 0.01 },
  { method: "GET", path: () => "/health", ms: 2, fail: 0 },
  { method: "POST", path: () => "/sessions", ms: 65, fail: 0.08 },
];
const BROWSER = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Safari/605.1.15";
const GOOGLEBOT = "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)";
const CLIENT = "ployz-shop-ios/3.2 CFNetwork/1494 Darwin/23.4.0";

/** A visitor's id for one UTC day, as the Collector would hash it: it changes with the day's salt. */
const visitor = (n: number, at: number) =>
  createHash("sha256").update(`visitor:${n}:${new Date(at).toISOString().slice(0, 10)}`).digest().readBigUInt64LE(0).toString();

type Utm = { utm_source?: string; utm_medium?: string; utm_campaign?: string; utm_term?: string; utm_content?: string };

type RequestWireRow = {
  ts: number; namespace: string; service: string; host: string; method: string; path: string; status: number; ua: string;
  query: Utm; duration_us: number; bytes_in: number; bytes_out: number; request_id: string;
  referrer_host: string | null; language: string | null; content_type: string | null; sec_purpose: string | null; visitor: string | null;
};

/** One row as `otel.ployz_request` holds it: optional text is '' when absent. */
const stored = (orgId: string, row: RequestWireRow) => ({
  OrgId: orgId, RetentionDays: 30, Timestamp: `${new Date(row.ts).toISOString().replace("T", " ").replace("Z", "")}000`,
  Namespace: row.namespace, Service: row.service, Host: row.host, Method: row.method, Path: row.path, Status: row.status,
  DurationUs: row.duration_us, BytesIn: row.bytes_in, BytesOut: row.bytes_out, RequestId: row.request_id, Ua: row.ua,
  ReferrerHost: row.referrer_host ?? "", Language: row.language ?? "", ContentType: row.content_type ?? "", SecPurpose: row.sec_purpose ?? "",
  Visitor: row.visitor, UtmSource: row.query.utm_source ?? "", UtmMedium: row.query.utm_medium ?? "", UtmCampaign: row.query.utm_campaign ?? "",
  UtmTerm: row.query.utm_term ?? "", UtmContent: row.query.utm_content ?? "", Ref: row.referrer_host ?? "",
});

/** One hour's requests: busier in the site's afternoon, and API errors burst every 9th hour. */
function hourRows(namespace: string, at: number, next: () => number): RequestWireRow[] {
  const hour = new Date(at).getUTCHours();
  const busy = 0.4 + 0.6 * Math.sin(((hour - 6) / 24) * Math.PI) ** 2;
  const burst = Math.floor(at / HOUR) % 9 === 0;
  const rows: RequestWireRow[] = [];
  const row = (fields: Partial<RequestWireRow> & Pick<RequestWireRow, "service" | "method" | "path" | "status" | "ua">) => {
    rows.push({
      ts: at + Math.floor(next() * HOUR), namespace, host: fields.service === "api" ? "api.shop.dev" : "shop.dev", query: {},
      duration_us: 5_000, bytes_in: 0, bytes_out: 2_048, request_id: `seed-${at}-${rows.length}`, referrer_host: null,
      language: null, content_type: null, sec_purpose: null, visitor: null, ...fields,
    });
  };

  for (let i = Math.round(14 * busy); i > 0; i--) {
    const source = SOURCES[Math.floor(next() * SOURCES.length)] ?? SOURCES[0]!;
    const missing = next() < 0.04;
    row({
      service: "web", method: "GET", path: missing ? "/old-pricing" : PAGES[Math.floor(next() * PAGES.length)] ?? "/",
      status: missing ? 404 : 200, ua: BROWSER, duration_us: Math.round((8 + next() * 40) * 1000), bytes_out: 24_000,
      content_type: "text/html; charset=utf-8", referrer_host: source.referrer, query: source.query,
      language: LANGUAGES[Math.floor(next() * LANGUAGES.length)] ?? null, visitor: visitor(Math.floor(next() * 400), at),
    });
  }
  if (next() < 0.5) row({ service: "web", method: "GET", path: "/sitemap.xml", status: 200, ua: GOOGLEBOT, content_type: "application/xml" });

  for (let i = Math.round(30 * busy); i > 0; i--) {
    const route = ROUTES[Math.floor(next() * ROUTES.length)] ?? ROUTES[0]!;
    const fail = next() < (burst && route.path(0) === "/checkout" ? 0.6 : route.fail);
    const status = fail ? (route.method === "POST" && next() < 0.5 ? 422 : next() < 0.5 ? 500 : 503) : 200;
    const spread = next() < 0.05 ? 6 : 0.5 + next();
    row({
      service: "api", method: route.method, path: route.path(1 + Math.floor(next() * 5000)), status, ua: CLIENT,
      duration_us: Math.round(route.ms * spread * 1000), bytes_in: route.method === "POST" ? 600 : 0,
      content_type: "application/json",
    });
  }
  return rows;
}

const seed = Effect.gen(function* () {
  const { drizzle } = yield* Database;
  const [environment] = yield* drizzle.execute<{ id: string; organizationId: string }>(sql`
    select e.id, e.organization_id as "organizationId"
    from config_environment e
    join config_project p on p.id = e.project_id
    join organization o on o.id::text = e.organization_id
    where o.slug = 'ada' and p.name = 'shop' and e.name = 'production'
  `, "objects");
  if (!environment) return yield* Effect.die("No ada/shop/production: run scripts/verify/up.sh first.");
  const { id: environmentId, organizationId } = environment;
  yield* setSiteAnalytics(organizationId, environmentId, { enabled: true, timezone: "UTC" });
  const namespace = (yield* namespacesByEnvironment(organizationId)).get(environmentId);
  if (namespace === undefined) return yield* Effect.die("ada/shop/production has no Namespace.");

  const next = random(7);
  const first = Math.floor(Date.now() / HOUR) * HOUR - DAYS * 24 * HOUR;
  const rows: RequestWireRow[] = [];
  for (let at = first; at <= Date.now(); at += HOUR) rows.push(...hourRows(namespace, at, next).filter((row) => row.ts <= Date.now()));

  const endpoint = yield* clickhouseEndpoint;
  if (endpoint === undefined) return yield* Effect.die("Set OBSERVE_CLICKHOUSE_URL in the Cloud's env to seed requests.");
  const target = new URL("/", endpoint.url);
  target.searchParams.set("query", "INSERT INTO otel.ployz_request FORMAT JSONEachRow");
  const authorization = `Basic ${Buffer.from(`${endpoint.user}:${Redacted.value(endpoint.password)}`).toString("base64")}`;
  const BATCH = 20_000;
  for (let from = 0; from < rows.length; from += BATCH) {
    const body = rows.slice(from, from + BATCH).map((row) => JSON.stringify(stored(organizationId, row))).join("\n");
    const response = yield* Effect.promise(() => fetch(target, { method: "POST", headers: { authorization }, body }));
    if (!response.ok) return yield* Effect.die(`ClickHouse refused the rows: ${(yield* Effect.promise(() => response.text())).slice(0, 300)}`);
  }
  return { environmentId, organizationId, namespace, inserted: rows.length };
});

export async function run() {
  console.log(`VERIFY_SEED_REQUESTS ${JSON.stringify(await AppRuntime.runPromise(seed))}`);
}
