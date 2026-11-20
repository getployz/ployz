import { randomUUID } from "node:crypto";
import { it } from "@effect/vitest";
import { Effect } from "effect";
import { afterAll, describe, expect } from "vitest";
import { testConfigLayer } from "#/test/config-environment";
import * as analytics from "../adapter";
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

const site = (organizationId: string): analytics.Site => ({ organizationId, environmentId: randomUUID(), timezone: "UTC", namespaces: [NAMESPACE] });
const query = (organizationId: string): analytics.Query => ({
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
});
