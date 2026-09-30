import { useSyncExternalStore } from "react";
import { formatDuration, formatRelativeTime } from "#/utils/relative-time";

const subscribe = () => () => {};
const everySecond = (tick: () => void) => {
  const timer = setInterval(tick, 1000);
  return () => clearInterval(timer);
};

/** "5 minutes ago" reads the clock, which moves between SSR and hydration (and a browser's clock may be off), so it renders only in the browser. */
export function RelativeTime({ date }: { date: Date }) {
  const client = useSyncExternalStore(subscribe, () => true, () => false);
  return <time dateTime={date.toISOString()} title={client ? date.toLocaleString() : undefined}>{client ? formatRelativeTime(date) : null}</time>;
}

/** How long something has run since `from`, ticking each second. It reads the clock, so it renders only in the browser. */
export function RunningTime({ from }: { from: Date }) {
  const now = useSyncExternalStore(everySecond, () => Math.floor(Date.now() / 1000), () => null);
  return now === null ? null : formatDuration(now - from.getTime() / 1000);
}
