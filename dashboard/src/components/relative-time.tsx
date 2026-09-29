import { useSyncExternalStore } from "react";
import { formatRelativeTime } from "#/utils/relative-time";

const subscribe = () => () => {};

/** "5 minutes ago" reads the clock, which moves between SSR and hydration (and a browser's clock may be off), so it renders only in the browser. */
export function RelativeTime({ date }: { date: Date }) {
  const client = useSyncExternalStore(subscribe, () => true, () => false);
  return <time dateTime={date.toISOString()} title={client ? date.toLocaleString() : undefined}>{client ? formatRelativeTime(date) : null}</time>;
}
