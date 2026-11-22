import { useEffect, useState } from "react";

/** How long a click reads as under way before its row arrives; past it, the request was lost. */
const PENDING_MS = 60_000;

/**
 * The request a click just sent, held until `arrived` finds its row in `latest` or the request is presumed lost. Cloud
 * may answer with another tab's attempt, so a caller can swap what the request waits on.
 */
export function usePendingAttempt<Requested, Latest>(latest: Latest, arrived: (requested: Requested, latest: Latest) => boolean) {
  const [requested, setRequested] = useState<Requested | null>(null);
  const hasArrived = requested !== null && arrived(requested, latest);

  useEffect(() => {
    if (hasArrived) setRequested(null);
  }, [hasArrived]);
  useEffect(() => {
    if (requested === null) return;
    const timer = setTimeout(() => setRequested(null), PENDING_MS);
    return () => clearTimeout(timer);
  }, [requested]);

  return [requested, setRequested, hasArrived] as const;
}
