import { dayStart, hourStart, localDay, SESSION_GAP_SECONDS } from "../local-time";
import type { BucketRow, EntryRow, Filter, FilterKey, GoalRow, Range, SessionDim, SessionRow, ValueRow } from "../model";

/** One stored pageview, in the shape of `analytics.pageview`. `visitorId` is the bigint's decimal text so JSON keeps it. */
export type PageviewRecord = {
  readonly ts: string;
  readonly visitorId: string;
  readonly host: string;
  readonly pathname: string;
  readonly source: string;
  readonly referrerHost: string | null;
  readonly utmSource: string | null;
  readonly utmMedium: string | null;
  readonly utmCampaign: string | null;
  readonly utmTerm: string | null;
  readonly utmContent: string | null;
  readonly ref: string | null;
  readonly language: string | null;
};

export type GoalRecord = { readonly id: string; readonly name: string; readonly pathPattern: string };

const COLUMN = {
  page: "pathname",
  host: "host",
  source: "source",
  utm_source: "utmSource",
  utm_medium: "utmMedium",
  utm_campaign: "utmCampaign",
  utm_term: "utmTerm",
  utm_content: "utmContent",
  language: "language",
} satisfies Record<FilterKey, keyof PageviewRecord>;

export const column = (row: PageviewRecord, key: FilterKey): string | null => row[COLUMN[key]];

/** A filter keeps the rows it names; `neq` also keeps rows with no value at all. */
export const selectRows = (rows: ReadonlyArray<PageviewRecord>, filters: ReadonlyArray<Filter>) =>
  rows.filter((row) => filters.every((filter) => (column(row, filter.key) === filter.value) === (filter.op === "eq")));

export type Session = {
  readonly visitorId: string;
  readonly first: PageviewRecord;
  readonly started: number;
  readonly ended: number;
  readonly pageviews: number;
};

const order = (a: PageviewRecord, b: PageviewRecord) =>
  Date.parse(a.ts) - Date.parse(b.ts) || compare(a.pathname, b.pathname) || compare(a.host, b.host);
const compare = (a: string, b: string) => (a < b ? -1 : a > b ? 1 : 0);

/** Per visitor, in `(ts, pathname, host)` order, a gap over 1800 s starts a new session. */
export function sessionize(rows: ReadonlyArray<PageviewRecord>): Session[] {
  const byVisitor = new Map<string, PageviewRecord[]>();
  for (const row of rows) byVisitor.set(row.visitorId, [...(byVisitor.get(row.visitorId) ?? []), row]);
  const sessions: Session[] = [];
  for (const [visitorId, visits] of byVisitor) {
    let current: Session | undefined;
    for (const row of [...visits].sort(order)) {
      const ts = Date.parse(row.ts);
      if (current !== undefined && ts - current.ended <= SESSION_GAP_SECONDS * 1000) {
        current = { ...current, ended: ts, pageviews: current.pageviews + 1 };
      } else {
        if (current !== undefined) sessions.push(current);
        current = { visitorId, first: row, started: ts, ended: ts, pageviews: 1 };
      }
    }
    if (current !== undefined) sessions.push(current);
  }
  return sessions;
}

const duration = (session: Session) => Math.floor((session.ended - session.started) / 1000);

/** Distinct visitors summed over site-local days, the only way a daily salt lets visitors add up. */
function visitorsByDay(timezone: string, items: ReadonlyArray<{ readonly at: number; readonly visitorId: string }>) {
  const days = new Map<string, Set<string>>();
  for (const item of items) {
    const day = localDay(timezone, item.at);
    days.set(day, (days.get(day) ?? new Set()).add(item.visitorId));
  }
  let total = 0;
  for (const set of days.values()) total += set.size;
  return total;
}

function groupBy<T>(items: ReadonlyArray<T>, key: (item: T) => string | null) {
  const groups = new Map<string, T[]>();
  for (const item of items) {
    const k = key(item);
    if (k === null) continue;
    groups.set(k, [...(groups.get(k) ?? []), item]);
  }
  return groups;
}

const sessionSums = (sessions: ReadonlyArray<Session>) => ({
  visits: sessions.length,
  bounces: sessions.filter((session) => session.pageviews === 1).length,
  totalDuration: sessions.reduce((sum, session) => sum + duration(session), 0),
});

/** `*` matches any run of characters; everything else is literal, as the store's escaped LIKE. */
export const goalMatcher = (pathPattern: string) => {
  const pattern = pathPattern.split("*").map((part) => part.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join(".*");
  const regex = new RegExp(`^${pattern}$`);
  return (pathname: string) => regex.test(pathname);
};

/**
 * The naive answer to every store question over `[range.start, range.end)`. Rows are selected by the filters first.
 * Sessions are built from the selected rows within `[start - 1800 s, end + 1 day)` and kept when they start in range.
 */
export class Reference {
  readonly rows: ReadonlyArray<PageviewRecord>;
  readonly sessions: ReadonlyArray<Session>;

  constructor(
    readonly timezone: string,
    all: ReadonlyArray<PageviewRecord>,
    readonly range: Range,
    filters: ReadonlyArray<Filter>,
  ) {
    const selected = selectRows(all, filters);
    const start = range.start.getTime();
    const end = range.end.getTime();
    this.rows = selected.filter((row) => Date.parse(row.ts) >= start && Date.parse(row.ts) < end);
    const window = selected.filter((row) => Date.parse(row.ts) >= start - SESSION_GAP_SECONDS * 1000 && Date.parse(row.ts) < end + 86_400_000);
    this.sessions = sessionize(window).filter((session) => session.started >= start && session.started < end);
  }

  private totals(bucketOf: (at: number) => Date): BucketRow[] {
    const rows = groupBy(this.rows, (row) => bucketOf(Date.parse(row.ts)).toISOString());
    const sessions = groupBy(this.sessions, (session) => bucketOf(session.started).toISOString());
    return [...new Set([...rows.keys(), ...sessions.keys()])].map((bucket) => {
      const inBucket = rows.get(bucket) ?? [];
      return {
        bucket: new Date(bucket),
        pageviews: inBucket.length,
        visitors: new Set(inBucket.map((row) => row.visitorId)).size,
        ...sessionSums(sessions.get(bucket) ?? []),
      };
    });
  }

  dailyTotals() {
    return this.totals((at) => dayStart(this.timezone, localDay(this.timezone, at)));
  }

  hourlyTotals() {
    return this.totals((at) => hourStart(this.timezone, new Date(at)));
  }

  breakdown(dim: FilterKey): ValueRow[] {
    return [...groupBy(this.rows, (row) => column(row, dim))].map(([value, rows]) => ({
      value,
      pageviews: rows.length,
      visitors: visitorsByDay(this.timezone, rows.map((row) => ({ at: Date.parse(row.ts), visitorId: row.visitorId }))),
    }));
  }

  sessionsBy(dim: SessionDim): SessionRow[] {
    return [...groupBy(this.sessions, (session) => column(session.first, dim))].map(([value, sessions]) => ({ value, ...sessionSums(sessions) }));
  }

  entryPages(): EntryRow[] {
    return [...groupBy(this.sessions, (session) => session.first.pathname)].map(([value, sessions]) => ({
      value,
      visitors: visitorsByDay(this.timezone, sessions.map((session) => ({ at: session.started, visitorId: session.visitorId }))),
      ...sessionSums(sessions),
    }));
  }

  goalConversions(goals: ReadonlyArray<GoalRecord>): GoalRow[] {
    return goals.map((goal) => {
      const matches = goalMatcher(goal.pathPattern);
      const hits = this.rows.filter((row) => matches(row.pathname));
      return {
        goalId: goal.id,
        name: goal.name,
        visitors: visitorsByDay(this.timezone, hits.map((row) => ({ at: Date.parse(row.ts), visitorId: row.visitorId }))),
        conversions: hits.length,
      };
    });
  }
}
