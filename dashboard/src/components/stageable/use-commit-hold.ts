import { useState } from "react";
import type { PersistableTransaction } from "./collection-field-resources";

/** Where a control's last write stands: running, refused with what it tried and why, or settled. */
export type CommitState<T> = { phase: "idle" } | { phase: "saving"; to: T } | { phase: "failed"; to: T; reason: string };

const IDLE = { phase: "idle" } as const;

/** A refusal's own words; a fetch that never reached Cloud says so instead of "Failed to fetch". */
function failureReason<E>(error: E) {
  return error instanceof Error && !(error instanceof TypeError) && error.message ? error.message : "Couldn't reach Ployz Cloud.";
}

/**
 * Shows the value a control last wrote until that write settles, over whatever the Store shows meanwhile. A settled
 * write has put its answer (or, refused, the reread) in the views, so `value` is right from then on. A refused write
 * stays `failed` with what it tried, so the control can offer it again, until the next write or `dismiss`.
 */
export function useCommitHold<T>(value: T) {
  const [state, setState] = useState<CommitState<T>>(IDLE);

  function hold(to: T, write: PersistableTransaction) {
    const saving = { phase: "saving", to } as const;
    setState(saving);
    const settle = (next: CommitState<T>) => setState((current) => (current === saving ? next : current));
    write.isPersisted.promise.then(
      () => settle(IDLE),
      (error) => settle({ phase: "failed", to, reason: failureReason(error) }),
    );
  }

  const dismiss = () => setState((current) => (current.phase === "failed" ? IDLE : current));

  return [state.phase === "saving" ? state.to : value, hold, state, dismiss] as const;
}
