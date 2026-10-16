// Seeds request rows for the verify stack's `shop/production` through the Machine push path, so Analytics shows Web,
// API and Errors. Turns analytics on for production in UTC, then pushes 30 days of requests as the fake Machine.
// Needs analytics on in Cloud: run up.sh with BILLING=1 (hosted), or set ANALYTICS_ENABLED=true.
// To run from dashboard/: node --env-file=.verify/run/env scripts/verify/runner.mjs /scripts/verify/seed-requests.ts
//   web   HTML pages with visitors, sources and campaigns, a few 404s, and Googlebot crawling
//   api   JSON routes with ids in their paths, a slow /reports/:id, and 5xx bursts on /checkout
// Rows land on fixed request ids and push seqs, so a rerun converges.
import { createHash } from "node:crypto";
import { sql } from "drizzle-orm";
import { Effect, Option } from "effect";
import { ingestRequestPush } from "#/modules/observe-ingest/ingest.server";
import { decodeRequestPush, MAX_PUSH_EVENTS, requestPushInput, type RequestWireRow } from "#/modules/observe-ingest/ingest";
import { namespacesByEnvironment, setSiteAnalytics } from "#/modules/web-analytics/salts.server";
import { Database } from "#/server/database.server";
import { AppRuntime } from "#/server/runtime.server";

const MACHINE_ID = "0123456789abcdef0123456789abcdef";
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
const SOURCES = [
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

const visitor = (n: number) => createHash("sha256").update(`visitor:${n}`).digest("hex");

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
      language: LANGUAGES[Math.floor(next() * LANGUAGES.length)] ?? null, visitor: visitor(Math.floor(next() * 400)),
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

  for (let seq = 0; seq * MAX_PUSH_EVENTS < rows.length; seq++) {
    const push = Option.flatMap(
      decodeRequestPush({ stream: "requests", seq: String(seq + 1), dropped: 0, rows: rows.slice(seq * MAX_PUSH_EVENTS, (seq + 1) * MAX_PUSH_EVENTS) }),
      requestPushInput,
    );
    if (Option.isNone(push)) return yield* Effect.die(`Push ${seq + 1} did not decode.`);
    yield* ingestRequestPush({ organizationId, machineId: MACHINE_ID as never }, push.value);
  }
  const [stored] = yield* drizzle.execute<{ count: number }>(sql`
    select count(*)::int as count from analytics.request where environment_id = ${environmentId}
  `, "objects");
  return { environmentId, pushed: rows.length, stored: stored?.count ?? 0 };
});

export async function run() {
  console.log(`VERIFY_SEED_REQUESTS ${JSON.stringify(await AppRuntime.runPromise(seed))}`);
}
