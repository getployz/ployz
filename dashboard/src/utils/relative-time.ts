const relativeTimeFormats = {
  long: new Intl.RelativeTimeFormat(undefined, { numeric: "auto" }),
  narrow: new Intl.RelativeTimeFormat(undefined, { numeric: "auto", style: "narrow" }),
};

const RELATIVE_UNITS: Array<[Intl.RelativeTimeFormatUnit, number]> = [
  ["year", 365 * 24 * 60 * 60 * 1000],
  ["month", 30 * 24 * 60 * 60 * 1000],
  ["day", 24 * 60 * 60 * 1000],
  ["hour", 60 * 60 * 1000],
  ["minute", 60 * 1000],
];

/** "56 minutes ago", "in 2 hours", "now" ("56m ago" when narrow) — no date library needed. */
export function formatRelativeTime(value: Date, now: Date = new Date(), style: keyof typeof relativeTimeFormats = "long"): string {
  const relativeTimeFormat = relativeTimeFormats[style];
  const diffMs = value.getTime() - now.getTime();
  const absMs = Math.abs(diffMs);

  for (const [unit, unitMs] of RELATIVE_UNITS) {
    if (absMs >= unitMs) {
      return relativeTimeFormat.format(Math.round(diffMs / unitMs), unit);
    }
  }

  return relativeTimeFormat.format(Math.round(diffMs / 1000), "second");
}
