// A fixed locale: the server and the browser must print the same words.
const relativeTimeFormat = new Intl.RelativeTimeFormat("en-US", {
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

/** How long something ran, whole seconds: "45s", "1m 12s", "2h 5m". */
export function formatDuration(seconds: number) {
  const whole = Math.max(0, Math.floor(seconds));
  if (whole < 60) return `${whole}s`;
  if (whole < 3600) return `${Math.floor(whole / 60)}m ${whole % 60}s`;
  return `${Math.floor(whole / 3600)}h ${Math.floor((whole % 3600) / 60)}m`;
}
