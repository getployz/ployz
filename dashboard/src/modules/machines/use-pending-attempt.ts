import { useEffect, useState } from "react";

/** How long a click reads as under way before its row arrives; past it, the request was lost. */
const PENDING_MS = 60_000;

/**
 * The request a click just sent, held until its row arrives in the Server's latest attempt (`latestId`) or the request
 * is presumed lost. `idOf` names the attempt a request waits on; Cloud may answer with another tab's, so it can change.
 */
export function usePendingAttempt<Requested>(latestId: string | undefined, idOf: (requested: Requested) => string) {
  const [requested, setRequested] = useState<Requested | null>(null);
  const arrived = requested !== null && latestId === idOf(requested);

  useEffect(() => {
    if (arrived) setRequested(null);
  }, [arrived]);
  useEffect(() => {
    if (requested === null) return;
    const timer = setTimeout(() => setRequested(null), PENDING_MS);
    return () => clearTimeout(timer);
  }, [requested]);

  return [requested, setRequested] as const;
}
