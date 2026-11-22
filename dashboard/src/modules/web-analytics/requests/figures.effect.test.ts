import { randomUUID } from "node:crypto";
import { it } from "@effect/vitest";
import { Effect } from "effect";
import { isbot } from "isbot";
import { afterAll, describe, expect } from "vitest";
import { testConfigLayer } from "#/test/config-environment";
import * as analytics from "../adapter";
import type { Query, Site } from "../model";
import { addDays, dayStart, localDay } from "../local-time";

const URL_UNSET = !process.env["OBSERVE_CLICKHOUSE_URL"];
const BROWSER = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";
const GOOGLEBOT = "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)";
const NAMESPACE = "shop-production";

const config = testConfigLayer({
  // Never connected: these reads touch ClickHouse only.
  DATABASE_URL: "postgres://localhost:1/unused",
  OBSERVE_CLICKHOUSE_URL: process.env["OBSERVE_CLICKHOUSE_URL"] ?? "",
  CLICKHOUSE_USER: process.env["CLICKHOUSE_USER"] ?? "default",
  CLICKHOUSE_PASSWORD: process.env["CLICKHOUSE_PASSWORD"] ?? "",
});

const clickhouse = async (query: string, body = "") => {
  const url = new URL("/", process.env["OBSERVE_CLICKHOUSE_URL"]);
  url.searchParams.set("query", query);
  const user = process.env["CLICKHOUSE_USER"] ?? "default";
  const response = await fetch(url, {
    method: "POST",
    headers: { authorization: `Basic ${Buffer.from(`${user}:${process.env["CLICKHOUSE_PASSWORD"] ?? ""}`).toString("base64")}` },
    body,
  });
  const text = await response.text();
  if (!response.ok) throw new Error(`ClickHouse answered ${response.status}: ${text.slice(0, 300)}`);
  return text;
};

type Seed = {
  readonly at: string;
  readonly path?: string;
  readonly status?: number;
  readonly visitor: number | null;
  readonly ua?: string;
  readonly contentType?: string;
  readonly method?: string;
  readonly service?: string;
  readonly secPurpose?: string;
};

const organizations: string[] = [];

/** Pageviews reach `otel.ployz_pageview` through its view. */
const seed = async (organizationId: string, prefix: string, rows: ReadonlyArray<Seed>, retentionDays = 30) => {
  organizations.push(organizationId);
  const body = rows.map((row, index) => JSON.stringify({
    OrgId: organizationId, RetentionDays: retentionDays, Timestamp: row.at.replace("T", " ").replace("Z", "") , Namespace: NAMESPACE,
    Service: row.service ?? "web", Host: "shop.test", Method: row.method ?? "GET", Path: row.path ?? "/", Status: row.status ?? 200,
    DurationUs: 12_000, BytesIn: 0, BytesOut: 100, RequestId: `${prefix}-${index}`, Ua: row.ua ?? BROWSER, ReferrerHost: "", Language: "en",
    ContentType: row.contentType ?? "text/html; charset=utf-8", SecPurpose: row.secPurpose ?? "", Visitor: row.visitor === null ? null : String(row.visitor),
    UtmSource: "", UtmMedium: "", UtmCampaign: "", UtmTerm: "", UtmContent: "", Ref: "",
  })).join("\n");
  await clickhouse("INSERT INTO otel.ployz_request FORMAT JSONEachRow", body);
};

/** What `otel.ployz_pageview` holds for one Organization, as `RetentionDays|Path|Visitor` lines (`\N` is no visitor). */
const pageviewRows = async (organizationId: string) => {
  const text = await clickhouse(`SELECT RetentionDays, Path, Visitor FROM otel.ployz_pageview WHERE OrgId = '${organizationId}' ORDER BY Path, Visitor, RequestId FORMAT TSV`);
  return text.trim().split("\n").filter((line) => line !== "").map((line) => line.replaceAll("\t", "|"));
};

afterAll(async () => {
  if (organizations.length === 0) return;
  const ids = organizations.map((id) => `'${id}'`).join(",");
  for (const table of ["ployz_request", "ployz_pageview"]) {
    await clickhouse(`ALTER TABLE otel.${table} DELETE WHERE OrgId IN (${ids}) SETTINGS mutations_sync = 2`);
  }
});

const today = localDay("UTC", Date.now());
const first = addDays("UTC", today, -3);
const second = addDays("UTC", today, -2);
const at = (day: string, time: string) => `${day}T${time}.000000Z`;

const site = (organizationId: string): Site => ({ organizationId, environmentId: randomUUID(), timezone: "UTC", namespaces: [NAMESPACE] });
const query = (organizationId: string): Query => ({
  site: site(organizationId),
  range: { start: dayStart("UTC", first), end: dayStart("UTC", today) },
  filters: [],
  limit: 10,
});

describe.skipIf(URL_UNSET)("request analytics on ClickHouse (needs OBSERVE_CLICKHOUSE_URL, CLICKHOUSE_USER and CLICKHOUSE_PASSWORD)", () => {
  const acme = randomUUID();
  const globex = randomUUID();

  it.live("counts unique visitors per day, leaves bots out, and keeps two Organizations in one namespace apart", () => Effect.gen(function* () {
    yield* Effect.promise(() => seed(acme, "acme", [
      { at: at(first, "09:00:00"), path: "/", visitor: 1 },
      { at: at(first, "09:10:00"), path: "/pricing", visitor: 1 },
      { at: at(first, "10:00:00"), path: "/", visitor: 2 },
      { at: at(first, "11:00:00"), path: "/", visitor: 99, ua: GOOGLEBOT },
      { at: at(first, "11:05:00"), path: "/data.json", visitor: 3, contentType: "application/json" },
      { at: at(first, "11:30:00"), path: "/prefetched", visitor: 4, secPurpose: "prefetch" },
      { at: at(first, "12:00:00"), path: "/checkout", visitor: null, method: "POST", status: 502, service: "api" },
      { at: at(second, "09:00:00"), path: "/", visitor: 5 },
      { at: at(second, "12:00:00"), path: "/", visitor: null },
    ]));
    yield* Effect.promise(() => seed(globex, "globex", [
      { at: at(first, "09:00:00"), path: "/", visitor: 11 },
      { at: at(first, "09:30:00"), path: "/globex-only", visitor: 12 },
      { at: at(first, "10:00:00"), path: "/", visitor: 13 },
      { at: at(first, "10:30:00"), path: "/", visitor: 14 },
      { at: at(second, "09:00:00"), path: "/globex-only", visitor: 15 },
      { at: at(first, "12:00:00"), path: "/globex-checkout", visitor: null, method: "POST", status: 500, service: "api" },
    ], 7));

    expect(yield* Effect.promise(() => pageviewRows(acme))).toEqual([
      "30|/|1", "30|/|2", "30|/|5", "30|/|99", "30|/|\\N", "30|/pricing|1",
    ]);
    expect(yield* Effect.promise(() => pageviewRows(globex))).toEqual([
      "7|/|11", "7|/|13", "7|/|14", "7|/globex-only|12", "7|/globex-only|15",
    ]);

    const mine = yield* analytics.figures(query(acme), "day", ["page"]);
    const theirs = yield* analytics.figures(query(globex), "day", ["page"]);
    expect(mine.noSaltDays).toEqual([second]);
    expect(theirs.noSaltDays).toEqual([]);

    const perDay = (answer: typeof mine) => answer.buckets.filter((b) => b.pageviews > 0).map(({ pageviews, visitors }) => ({ pageviews, visitors }));
    expect(perDay(mine)).toEqual([{ pageviews: 3, visitors: 2 }, { pageviews: 1, visitors: 1 }]);
    expect(perDay(theirs)).toEqual([{ pageviews: 4, visitors: 4 }, { pageviews: 1, visitors: 1 }]);
    expect(mine.values("page").map((row) => row.value).sort()).toEqual(["/", "/pricing"]);
    expect(theirs.values("page").map((row) => row.value).sort()).toEqual(["/", "/globex-only"]);

    const myApi = yield* analytics.api(query(acme));
    const theirApi = yield* analytics.api(query(globex));
    expect(myApi.routes.map((row) => row.route)).not.toContain("/globex-checkout");
    expect(myApi.routes.map((row) => row.route)).not.toContain("/globex-only");
    expect(theirApi.routes.map((row) => row.route)).not.toContain("/pricing");
    const myErrors = yield* analytics.errors(query(acme));
    expect(myErrors.byRoute).toEqual([{ route: "/checkout", status: 502, requests: 1 }]);
    expect(myErrors.services).toEqual(["api", "web"]);

    expect((yield* analytics.request(site(globex), "globex-0"))?.path).toBe("/");
    expect(yield* analytics.request(site(acme), "globex-0")).toBeNull();
  }).pipe(Effect.provide(config)), 60_000);
  it.live("folds a visitor's pageviews into visits, with bounces, durations, entry pages, goals and a page filter", () => Effect.gen(function* () {
    const org = randomUUID();
    yield* Effect.promise(() => seed(org, "visits", [
      { at: at(first, "09:00:00"), path: "/", visitor: 1 },
      { at: at(first, "09:10:00"), path: "/pricing", visitor: 1 },
      { at: at(first, "11:00:00"), path: "/docs", visitor: 1 },
      { at: at(first, "10:00:00"), path: "/pricing", visitor: 2 },
    ]));
    const base = { ...query(org), goals: [{ id: "g1", name: "Pricing", pathPattern: "/pric*" }] };

    const all = yield* analytics.figures(base, "day", ["page"]);
    expect(all.overview).toMatchObject({ pageviews: 4, visitors: 2, visits: 3, bounces: 2, totalDuration: 600, bounceRatePercent: 66.7, avgDurationSeconds: 200 });
    expect(all.entryPages.map(({ value, visits }) => [value, visits])).toEqual([["/", 1], ["/docs", 1], ["/pricing", 1]]);
    expect(all.goals).toEqual([{ goalId: "g1", name: "Pricing", visitors: 2, conversions: 2 }]);

    const filtered = yield* analytics.figures({ ...base, filters: [{ key: "page", op: "eq", value: "/pricing" }] }, "day", ["page"]);
    expect(filtered.overview).toMatchObject({ pageviews: 2, visitors: 2, visits: 2 });
    expect(filtered.values("page")).toEqual([{ value: "/pricing", pageviews: 2, visitors: 2 }]);

    const hourly = yield* analytics.figures(base, "hour", []);
    expect(hourly.buckets.filter((bucket) => bucket.pageviews > 0).map((bucket) => [bucket.bucket.toISOString().slice(11, 13), bucket.pageviews, bucket.visits]))
      .toEqual([["09", 2, 1], ["10", 1, 1], ["11", 1, 1]]);
  }).pipe(Effect.provide(config)), 60_000);
  it.live("matches a goal whose pattern holds an underscore, a percent sign or a backslash, and only that text", () => Effect.gen(function* () {
    const org = randomUUID();
    yield* Effect.promise(() => seed(org, "goal-text", [
      { at: at(first, "09:00:00"), path: "/sign_up", visitor: 1 },
      { at: at(first, "09:01:00"), path: "/signXup", visitor: 2 },
      { at: at(first, "09:02:00"), path: "/50%", visitor: 3 },
      { at: at(first, "09:03:00"), path: "/500", visitor: 4 },
      { at: at(first, "09:04:00"), path: "/a\\b", visitor: 5 },
    ]));
    const goals = [
      { id: "us", name: "Underscore", pathPattern: "/sign_up" },
      { id: "pc", name: "Percent", pathPattern: "/50%" },
      { id: "bs", name: "Backslash", pathPattern: "/a\\b" },
    ];
    const answer = yield* analytics.figures({ ...query(org), goals }, "day", []);
    expect(Object.fromEntries(answer.goals.map(({ goalId, visitors }) => [goalId, visitors]))).toEqual({ us: 1, pc: 1, bs: 1 });
  }).pipe(Effect.provide(config)), 60_000);
  it.live("filters to a page whose path holds a quote or a backslash", () => Effect.gen(function* () {
    const org = randomUUID();
    yield* Effect.promise(() => seed(org, "page-text", [
      { at: at(first, "09:00:00"), path: "/O'Brien", visitor: 1 },
      { at: at(first, "09:01:00"), path: "/a\\b", visitor: 2 },
      { at: at(first, "09:02:00"), path: "/", visitor: 3 },
    ]));
    for (const path of ["/O'Brien", "/a\\b"]) {
      const filtered = yield* analytics.figures({ ...query(org), filters: [{ key: "page", op: "eq", value: path }] }, "day", ["page"]);
      expect(filtered.overview.pageviews).toBe(1);
      expect(filtered.values("page")).toEqual([{ value: path, pageviews: 1, visitors: 1 }]);
    }
  }).pipe(Effect.provide(config)), 60_000);
  it.live("reads thousands of distinct crawler agents and counts none of them", () => Effect.gen(function* () {
    const org = randomUUID();
    yield* Effect.promise(() => seed(org, "crawlers", Array.from({ length: 8192 }, (_, i) => ({ at: at(first, "09:00:00"), visitor: i + 1, ua: `${GOOGLEBOT} ${i}` }))));
    const answer = yield* analytics.figures(query(org), "day", ["page"]);
    expect(answer.overview.pageviews).toBe(0);
    expect((yield* analytics.api(query(org))).routes).toEqual([]);
  }).pipe(Effect.provide(config)), 60_000);
  it.live("leaves out a crawler however many other agents the range holds", () => Effect.gen(function* () {
    const org = randomUUID();
    yield* Effect.promise(() => seed(org, "crowd", Array.from({ length: 20_001 }, (_, i) => ({
      at: at(first, "09:00:00"), path: i === 20_000 ? "/crawler" : "/", visitor: i + 1, ua: i === 20_000 ? "zzz Googlebot/2.1" : `${BROWSER} ${i}`,
    }))));
    const answer = yield* analytics.figures(query(org), "day", ["page"]);
    expect(answer.overview.pageviews).toBe(20_000);
    expect(answer.values("page").map((row) => row.value)).toEqual(["/"]);
  }).pipe(Effect.provide(config)), 60_000);
  it.live("calls the agents bots that isbot calls bots", () => Effect.gen(function* () {
    const fragments = [
      "google", "Google", "news", "sapphire", "wv", "app", "/google", " pixel", " channel/", "google/", "bot", "bots", "Bot_", " cu", "cam", "scan",
      "lib", "http", ";", "java", "x", "Mozilla/5.0 ", " ", "/", "(", ")", "1.0", "a", "curl/8", "\n", "é", "search",
    ];
    let state = 12345;
    const next = (bound: number) => {
      state = (Math.imul(state, 1103515245) + 12345) >>> 0;
      return state % bound;
    };
    const agents = [
      "", BROWSER, GOOGLEBOT, "curl/8.5.0", "Mozilla/5.0 (Linux; Android 10; Pixel 4) AppleWebKit/537.36 Chrome/141.0 Mobile Safari/537.36",
      "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) GSA/300.0 Mobile/15E148 Safari/604.1",
      // Each of these ends a browser agent where only one of isbot's lookaround patterns decides.
      ...[" newssapphirex", " newsfoo", " camscancam", " libhttpx", " cam cubot", " cubot", " libjava;", " wvjava;", " java;x", " googlewv", " google pixel",
        " googleapp", " channel/google/_", " ;google/google", " googlex"].map((tail) => BROWSER + tail),
      ...Array.from({ length: 4000 }, (_, i) => (i % 2 === 0 ? BROWSER + " " : "") + Array.from({ length: 1 + next(6) }, () => fragments[next(fragments.length)]).join("")),
    ];
    const org = randomUUID();
    yield* Effect.promise(() => seed(org, "parity", agents.map((ua, i) => ({ at: at(first, "09:00:00"), path: `/${i}`, visitor: i + 1, ua }))));
    const answer = yield* analytics.figures({ ...query(org), limit: agents.length }, "day", ["page"]);
    const counted = new Set(answer.values("page").map((row) => row.value));
    const disagree = agents.flatMap((ua, i) => (counted.has(`/${i}`) === isbot(ua) ? [ua] : []));
    expect(disagree).toEqual([]);
    expect(counted.size).toBeGreaterThan(100);
    expect(counted.size).toBeLessThan(agents.length - 100);
  }).pipe(Effect.provide(config)), 60_000);
  it.live("lists every Service that took a request, before and after one is picked", () => Effect.gen(function* () {
    const org = randomUUID();
    yield* Effect.promise(() => seed(org, "services", [
      { at: at(first, "09:00:00"), path: "/v1/users", visitor: null, service: "api", status: 500 },
      { at: at(first, "09:01:00"), path: "/", visitor: null, service: "web" },
    ]));
    const q = query(org);
    for (const read of [analytics.api, analytics.errors]) {
      expect((yield* read(q)).services).toEqual(["api", "web"]);
      const picked = yield* read(q, "api");
      expect(picked.services).toEqual(["api", "web"]);
    }
    expect((yield* analytics.api(q, "api")).routes.map((row) => row.route)).toEqual(["/v1/users"]);
  }).pipe(Effect.provide(config)), 60_000);
  it.live("reads a goal whose pattern holds a quote", () => Effect.gen(function* () {
    const org = randomUUID();
    yield* Effect.promise(() => seed(org, "quote", [{ at: at(first, "09:00:00"), path: "/it's here", visitor: 1 }]));
    const answer = yield* analytics.figures({ ...query(org), goals: [{ id: "q", name: "Quote", pathPattern: "/it's*" }] }, "day", []);
    expect(answer.goals).toEqual([{ goalId: "q", name: "Quote", visitors: 1, conversions: 1 }]);
  }).pipe(Effect.provide(config)), 60_000);
});
