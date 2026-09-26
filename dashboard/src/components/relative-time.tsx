import { formatRelativeTime } from "#/utils/relative-time";

/** "5 minutes ago" reads the clock, which moves between SSR and hydration (and a browser's clock may be off); the server's text stands until the next render. */
export function RelativeTime({ date }: { date: Date }) {
  return <time dateTime={date.toISOString()} suppressHydrationWarning>{formatRelativeTime(date)}</time>;
}
