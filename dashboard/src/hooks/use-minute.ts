import { useSyncExternalStore } from "react";

const everyMinute = (tick: () => void) => {
  const timer = setInterval(tick, 60_000);
  return () => clearInterval(timer);
};

/**
 * The clock, read again each minute so ages move and a time from yesterday gains its date after midnight. It moves
 * between SSR and hydration, so it is null on the server.
 */
export function useMinute(): Date | null {
  const minute = useSyncExternalStore(everyMinute, () => Math.floor(Date.now() / 60_000), () => null);
  return minute === null ? null : new Date();
}
