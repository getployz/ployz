const relativeTimeFormat = new Intl.RelativeTimeFormat(undefined, {
  numeric: "auto",
});

const RELATIVE_UNITS: Array<[Intl.RelativeTimeFormatUnit, number]> = [
  ["year", 365 * 24 * 60 * 60 * 1000],
  ["month", 30 * 24 * 60 * 60 * 1000],
  ["day", 24 * 60 * 60 * 1000],
  ["hour", 60 * 60 * 1000],
  ["minute", 60 * 1000],
];

/** "56 minutes ago", "in 2 hours", "now" — no date library needed. */
export function formatRelativeTime(value: Date, now: Date = new Date()): string {
  const diffMs = value.getTime() - now.getTime();
  const absMs = Math.abs(diffMs);

  for (const [unit, unitMs] of RELATIVE_UNITS) {
    if (absMs >= unitMs) {
      return relativeTimeFormat.format(Math.round(diffMs / unitMs), unit);
    }
  }

  return relativeTimeFormat.format(Math.round(diffMs / 1000), "second");
}
