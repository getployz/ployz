import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dayEnd, dayStart, isHourStart, localDays } from "../local-time";
import type { Filter } from "../model";
import { expectedOf, type Fixture, type FixtureSite, type QuerySpec, serialized } from "./fixture";
import type { GoalRecord, PageviewRecord } from "./reference";

type Rng = () => number;

const mulberry32 = (seed: number): Rng => {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) | 0;
    let t = Math.imul(state ^ (state >>> 15), 1 | state);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
};

const pick = <T>(rng: Rng, items: readonly [T, ...T[]]): T => items[Math.floor(rng() * items.length)] ?? items[0];
const chance = (rng: Rng, probability: number) => rng() < probability;

const PAGES = ["/", "/", "/", "/pricing", "/pricing", "/docs", "/docs/install", "/docs/api", "/blog", "/blog/launch", "/blog/launch_2026",
  "/blog/launchX2026", "/signup", "/about", "/contact"] as const;
const HOSTS = ["acme.example", "www.acme.example"] as const;
const SOURCES = ["direct", "direct", "google", "google", "twitter", "github", "newsletter"] as const;
const REFERRERS = { direct: null, google: "www.google.com", twitter: "t.co", github: "github.com", newsletter: null } satisfies Record<(typeof SOURCES)[number], string | null>;
const LANGUAGES = [null, "en", "en", "en-AU", "de", "fr"] as const;
type Campaign = Pick<PageviewRecord, "utmSource" | "utmMedium" | "utmCampaign" | "utmTerm" | "utmContent" | "ref">;
const NONE: Campaign = { utmSource: null, utmMedium: null, utmCampaign: null, utmTerm: null, utmContent: null, ref: null };
const CAMPAIGNS: readonly [Campaign, ...Campaign[]] = [
  { ...NONE, utmSource: "newsletter", utmMedium: "email", utmCampaign: "launch" },
  { ...NONE, utmSource: "twitter", utmMedium: "social", utmCampaign: "launch", utmContent: "banner" },
  { ...NONE, utmSource: "partner", utmMedium: "referral", utmCampaign: "spring", utmTerm: "cheap" },
  { ...NONE, ref: "hn" },
];
const GAPS = [0, 1_500, 5_000, 30_000, 59_999, 120_000, 600_000, 1_200_000, 1_800_000, 1_801_000] as const;

type Visitor = { readonly id: string; readonly host: string; readonly language: string | null };

const visitorId = (rng: Rng) =>
  BigInt.asIntN(64, (BigInt(Math.floor(rng() * 2 ** 32)) << 32n) | BigInt(Math.floor(rng() * 2 ** 32))).toString();

const visitor = (rng: Rng): Visitor => ({ id: visitorId(rng), host: pick(rng, HOSTS), language: pick(rng, LANGUAGES) });

const row = (ts: number, who: Visitor, pathname: string, source: (typeof SOURCES)[number], campaign: Campaign, host = who.host): PageviewRecord => ({
  ts: new Date(ts).toISOString(),
  visitorId: who.id,
  host,
  pathname,
  source,
  referrerHost: REFERRERS[source],
  ...campaign,
  language: who.language,
});

function session(rng: Rng, who: Visitor, start: number, pageviews: number, out: PageviewRecord[]) {
  const source = pick(rng, SOURCES);
  const campaign = chance(rng, 0.3) ? pick(rng, CAMPAIGNS) : NONE;
  let at = start;
  for (let n = 0; n < pageviews; n++) {
    if (n > 0) at += pick(rng, GAPS);
    out.push(row(at, who, pick(rng, PAGES), source, campaign));
  }
}

type SiteProfile = { readonly site: FixtureSite; readonly firstDay: string; readonly now: string; readonly visitors: number; readonly seed: number };

function ties(who: Visitor, at: number, out: PageviewRecord[]) {
  out.push(row(at, who, "/pricing", "google", NONE), row(at, who, "/about", "google", NONE));
  out.push(row(at + 60_000, who, "/docs", "google", NONE, HOSTS[0]), row(at + 60_000, who, "/docs", "google", NONE, HOSTS[1]));
}

function rowsFor(profile: SiteProfile): PageviewRecord[] {
  const rng = mulberry32(profile.seed);
  const { timezone } = profile.site;
  const now = Date.parse(profile.now);
  const visitors = Array.from({ length: profile.visitors }, () => visitor(rng));
  const out: PageviewRecord[] = [];
  for (const day of localDays(timezone, dayStart(timezone, profile.firstDay), new Date(now))) {
    const start = dayStart(timezone, day).getTime();
    const end = dayEnd(timezone, day).getTime();
    visitors.forEach((who, index) => {
      if (index === 0) out.push(row(start, who, "/", "direct", NONE));
      if (index === 1) session(rng, who, end - 20 * 60_000, 3, out);
      if (index === 2 && chance(rng, 0.5)) ties(who, start + Math.floor(rng() * (end - start)), out);
      if (chance(rng, 0.4)) return;
      for (let n = chance(rng, 0.7) ? 1 : 2; n > 0; n--) {
        const at = start + Math.floor(rng() * (end - start) / 1000) * 1000 + (chance(rng, 0.3) ? Math.floor(rng() * 1000) : 0);
        session(rng, who, at, 1 + Math.floor(rng() * 5), out);
      }
    });
  }
  const seen = new Set<string>();
  return out
    .filter((record) => Date.parse(record.ts) < now)
    .filter((record) => {
      const key = [record.ts, record.visitorId, record.host, record.pathname].join("|");
      return seen.has(key) ? false : (seen.add(key), true);
    })
    .sort((a, b) => Date.parse(a.ts) - Date.parse(b.ts) || a.visitorId.localeCompare(b.visitorId) || a.pathname.localeCompare(b.pathname));
}

/** Goal ids end in b, the site id's last two digits, and the goal's number, so fixtures seeded together never share one. */
const goalsFor = (environmentId: string): ReadonlyArray<GoalRecord> => [
  { name: "Signup", pathPattern: "/signup" },
  { name: "Docs", pathPattern: "/docs/*" },
  { name: "Launch post", pathPattern: "/blog/launch_2026" },
  { name: "Never", pathPattern: "/nowhere" },
].map((goal, index) => ({ id: `${environmentId.slice(0, -4)}b${environmentId.slice(-2)}${index + 1}`, ...goal }));

const eq = (key: Filter["key"], value: string): Filter => ({ key, op: "eq", value });
const neq = (key: Filter["key"], value: string): Filter => ({ key, op: "neq", value });

type Ranges = Record<"whole" | "edges" | "closed" | "today" | "hours", readonly [string, string]>;

/** `Omit` over the union collapses it to the common keys, so each variant is cut down on its own. */
type Variant<S> = S extends QuerySpec ? Omit<S, "name" | "range" | "filters"> : never;

function specs(ranges: Ranges): QuerySpec[] {
  const spec = (name: string, range: keyof Ranges, rest: Variant<QuerySpec>, filters: Filter[] = []): QuerySpec =>
    ({ name, range: ranges[range], filters, ...rest });
  return [
    spec("overview whole", "whole", { kind: "overview" }),
    spec("overview edges", "edges", { kind: "overview" }),
    spec("overview closed days", "closed", { kind: "overview" }),
    spec("overview today", "today", { kind: "overview" }),
    spec("overview from google", "whole", { kind: "overview" }, [eq("source", "google")]),
    spec("overview not english", "edges", { kind: "overview" }, [neq("language", "en")]),
    spec("overview pricing page", "whole", { kind: "overview" }, [eq("page", "/pricing")]),
    spec("overview two filters", "whole", { kind: "overview" }, [eq("host", "www.acme.example"), neq("utm_source", "newsletter")]),
    spec("days whole", "whole", { kind: "timeseries", interval: "day" }),
    spec("days edges", "edges", { kind: "timeseries", interval: "day" }),
    spec("hours", "hours", { kind: "timeseries", interval: "hour" }),
    spec("hours closed day", "closed", { kind: "timeseries", interval: "hour" }),
    spec("hours from google", "hours", { kind: "timeseries", interval: "hour" }, [eq("source", "google")]),
    spec("pages top 5", "whole", { kind: "breakdown", dim: "page", limit: 5 }),
    spec("sources top 3 edges", "edges", { kind: "breakdown", dim: "source", limit: 3 }),
    spec("utm sources", "whole", { kind: "breakdown", dim: "utm_source", limit: 10 }),
    spec("languages on www", "whole", { kind: "breakdown", dim: "language", limit: 10 }, [eq("host", "www.acme.example")]),
    spec("hosts closed days", "closed", { kind: "breakdown", dim: "host", limit: 10 }),
    spec("campaigns top 1", "whole", { kind: "breakdown", dim: "utm_campaign", limit: 1 }),
    spec("sessions by source", "whole", { kind: "sessions", dim: "source", limit: 10 }),
    spec("sessions by host top 1 edges", "edges", { kind: "sessions", dim: "host", limit: 1 }),
    spec("sessions by medium not english", "whole", { kind: "sessions", dim: "utm_medium", limit: 10 }, [neq("language", "en")]),
    spec("sessions by language today", "today", { kind: "sessions", dim: "language", limit: 10 }),
    spec("entry pages top 4", "whole", { kind: "entryPages", limit: 4 }),
    spec("entry pages edges", "edges", { kind: "entryPages", limit: 10 }),
    spec("entry pages from google", "whole", { kind: "entryPages", limit: 10 }, [eq("source", "google")]),
    spec("goals whole", "whole", { kind: "goalConversions" }),
    spec("goals on www edges", "edges", { kind: "goalConversions" }, [eq("host", "www.acme.example")]),
    spec("goals today", "today", { kind: "goalConversions" }),
  ];
}

function fixture(profile: SiteProfile, ranges: Ranges): Fixture {
  for (const [start, end] of Object.values(ranges)) {
    for (const bound of [start, end]) {
      if (!isHourStart(profile.site.timezone, new Date(bound))) throw new Error(`${bound} is not a ${profile.site.timezone} hour start`);
    }
  }
  const rows = rowsFor(profile);
  const goals = goalsFor(profile.site.environmentId);
  return {
    now: profile.now,
    site: profile.site,
    rows,
    goals,
    queries: specs(ranges).map((spec) => ({ spec, expected: serialized(expectedOf(spec, profile.site.timezone, rows, goals)) })),
  };
}

const utc = fixture(
  { site: { environmentId: "00000000-0000-4000-8000-00000000a001", timezone: "UTC" }, firstDay: "2026-10-08", now: "2026-10-15T10:30:00.000Z", visitors: 40, seed: 20261008 },
  {
    whole: ["2026-10-08T00:00:00.000Z", "2026-10-15T11:00:00.000Z"],
    edges: ["2026-10-09T13:00:00.000Z", "2026-10-13T05:00:00.000Z"],
    closed: ["2026-10-10T00:00:00.000Z", "2026-10-12T00:00:00.000Z"],
    today: ["2026-10-15T02:00:00.000Z", "2026-10-15T09:00:00.000Z"],
    hours: ["2026-10-14T20:00:00.000Z", "2026-10-15T06:00:00.000Z"],
  },
);

const sydney = fixture(
  { site: { environmentId: "00000000-0000-4000-8000-00000000a002", timezone: "Australia/Sydney" }, firstDay: "2026-10-02", now: "2026-10-06T05:30:00.000Z", visitors: 30, seed: 20261002 },
  {
    whole: ["2026-10-01T14:00:00.000Z", "2026-10-06T06:00:00.000Z"],
    edges: ["2026-10-02T03:00:00.000Z", "2026-10-05T02:00:00.000Z"],
    closed: ["2026-10-03T14:00:00.000Z", "2026-10-04T13:00:00.000Z"],
    today: ["2026-10-05T20:00:00.000Z", "2026-10-06T06:00:00.000Z"],
    hours: ["2026-10-04T10:00:00.000Z", "2026-10-06T06:00:00.000Z"],
  },
);

const lines = (items: ReadonlyArray<object>) => `[\n${items.map((item) => `    ${JSON.stringify(item)}`).join(",\n")}\n  ]`;
const render = (data: Fixture) =>
  `{\n  "now": ${JSON.stringify(data.now)},\n  "site": ${JSON.stringify(data.site)},\n  "goals": ${lines(data.goals)},\n`
  + `  "rows": ${lines(data.rows)},\n  "queries": ${lines(data.queries)}\n}\n`;

export const FIXTURES = { utc, sydney } as const;

export async function run() {
  for (const [name, data] of Object.entries(FIXTURES)) {
    const path = fileURLToPath(new URL(`./${name}.json`, import.meta.url));
    writeFileSync(path, render(data));
    console.log(`${path}: ${data.rows.length} rows, ${data.queries.length} queries`);
  }
}
