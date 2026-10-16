export const RETENTION_DAYS = 30;

export type Site = { readonly environmentId: string; readonly timezone: string };

/** Half-open `[start, end)` in UTC instants. Both bounds sit on site-local hour starts. */
export type Range = { readonly start: Date; readonly end: Date };

export const FILTER_KEYS = ["page", "host", "source", "utm_source", "utm_medium", "utm_campaign", "utm_term", "utm_content", "language"] as const;
export type FilterKey = (typeof FILTER_KEYS)[number];

/** The dims a session is attributed by: its first pageview's value. `page` is not one; a session has many pages. */
export const SESSION_DIMS = ["source", "utm_source", "utm_medium", "utm_campaign", "utm_term", "utm_content", "host", "language"] as const;
export type SessionDim = (typeof SESSION_DIMS)[number];

export type Filter = { readonly key: FilterKey; readonly op: "eq" | "neq"; readonly value: string };

export type Query = { readonly site: Site; readonly range: Range; readonly filters: ReadonlyArray<Filter>; readonly limit: number };

export type Interval = "hour" | "day";

export type Totals = {
  readonly pageviews: number;
  readonly visitors: number;
  readonly visits: number;
  readonly bounces: number;
  readonly totalDuration: number;
};
export type BucketRow = Totals & { readonly bucket: Date };
export type ValueRow = { readonly value: string; readonly pageviews: number; readonly visitors: number };
export type SessionRow = { readonly value: string; readonly visits: number; readonly bounces: number; readonly totalDuration: number };
export type EntryRow = SessionRow & { readonly visitors: number };
export type GoalRow = { readonly goalId: string; readonly name: string; readonly visitors: number; readonly conversions: number };

export const ZERO_TOTALS: Totals = { pageviews: 0, visitors: 0, visits: 0, bounces: 0, totalDuration: 0 };
