import { dayStart, ENTRY_LOOKBACK_SECONDS, hourStart, localDay, SESSION_GAP_SECONDS } from "../local-time";
import { type BucketRow, type EntryRow, type Filter, type FilterKey, type GoalRow, type Range, SESSION_DIMS, type SessionDim, type SessionRow, type ValueRow } from "../model";

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

const matches = (row: PageviewRecord, filters: ReadonlyArray<Filter>) =>
  filters.every((filter) => (column(row, filter.key) === filter.value) === (filter.op === "eq"));

const SESSION_KEYS: ReadonlySet<FilterKey> = new Set(SESSION_DIMS);
const isSessionDim = (key: FilterKey): key is SessionDim => SESSION_KEYS.has(key);

export type Session = {
  readonly visitorId: string;
  readonly first: PageviewRecord;
  readonly started: number;
  readonly ended: number;
  readonly rows: ReadonlyArray<PageviewRecord>;
};

/** The pageview as its visit sees it: its own page, and every session dim taken from the visit's first pageview. */
const attributed = (row: PageviewRecord, first: PageviewRecord): PageviewRecord => ({
  ...row,
  host: first.host,
  source: first.source,
  utmSource: first.utmSource,
  utmMedium: first.utmMedium,
  utmCampaign: first.utmCampaign,
  utmTerm: first.utmTerm,
  utmContent: first.utmContent,
  language: first.language,
});

const order = (a: PageviewRecord, b: PageviewRecord) =>
  Date.parse(a.ts) - Date.parse(b.ts) || compare(a.pathname, b.pathname) || compare(a.host, b.host);
const compare = (a: string, b: string) => (a < b ? -1 : a > b ? 1 : 0);

export function sessionize(rows: ReadonlyArray<PageviewRecord>): Session[] {
  const byVisitor = new Map<string, PageviewRecord[]>();
  for (const row of rows) byVisitor.set(row.visitorId, [...(byVisitor.get(row.visitorId) ?? []), row]);
  const sessions: Session[] = [];
  for (const [visitorId, visits] of byVisitor) {
    let current: Session | undefined;
    for (const row of [...visits].sort(order)) {
      const ts = Date.parse(row.ts);
      if (current !== undefined && ts - current.ended <= SESSION_GAP_SECONDS * 1000) {
        current = { ...current, ended: ts, rows: [...current.rows, row] };
      } else {
        if (current !== undefined) sessions.push(current);
        current = { visitorId, first: row, started: ts, ended: ts, rows: [row] };
      }
    }
    if (current !== undefined) sessions.push(current);
  }
  return sessions;
}

const duration = (session: Session) => Math.floor((session.ended - session.started) / 1000);

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
  bounces: sessions.filter((session) => session.rows.length === 1).length,
  totalDuration: sessions.reduce((sum, session) => sum + duration(session), 0),
});

export const goalMatcher = (pathPattern: string) => {
  const pattern = pathPattern.split("*").map((part) => part.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join(".*");
  const regex = new RegExp(`^${pattern}$`);
  return (pathname: string) => regex.test(pathname);
};

/**
 * Filters select visits, then pageviews inside them. A session dim filter keeps the visits whose first pageview has the
 * value; a page filter keeps the pageviews on that page, and the visits holding one. Pageviews and visitors count the
 * selected pageviews; visits, bounces and duration count the selected visits whole. A visit is followed back at most
 * {@link ENTRY_LOOKBACK_SECONDS} before the range for its entry, and a day past it for its end.
 */
export class Reference {
  /** The selected pageviews in the range, each attributed to its visit by {@link attributed}. */
  readonly rows: ReadonlyArray<PageviewRecord>;
  /** The selected visits that start in the range. */
  readonly sessions: ReadonlyArray<Session>;

  constructor(
    readonly timezone: string,
    all: ReadonlyArray<PageviewRecord>,
    readonly range: Range,
    filters: ReadonlyArray<Filter>,
  ) {
    const start = range.start.getTime();
    const end = range.end.getTime();
    const sessionFilters = filters.filter((filter) => isSessionDim(filter.key));
    const pageFilters = filters.filter((filter) => !isSessionDim(filter.key));
    const window = all.filter((row) => Date.parse(row.ts) >= start - ENTRY_LOOKBACK_SECONDS * 1000 && Date.parse(row.ts) < end + 86_400_000);
    const selected = sessionize(window)
      .filter((session) => matches(session.first, sessionFilters))
      .map((session) => ({ session, picked: session.rows.filter((row) => matches(row, pageFilters)) }))
      .filter(({ picked }) => picked.length > 0);
    this.rows = selected.flatMap(({ session, picked }) =>
      picked.filter((row) => Date.parse(row.ts) >= start && Date.parse(row.ts) < end).map((row) => attributed(row, session.first)));
    this.sessions = selected.map(({ session }) => session).filter((session) => session.started >= start && session.started < end);
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
