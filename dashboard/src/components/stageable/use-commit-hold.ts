import { useState } from "react";
import type { PersistableTransaction } from "./collection-field-resources";

/**
 * Shows the value a control last wrote until that write settles, over whatever the Store shows meanwhile. A settled
 * write has put its answer (or, refused, the reread) in the views, so `value` is right from then on.
 */
export function useCommitHold<T>(value: T) {
  const [held, setHeld] = useState<{ to: T } | null>(null);

  function hold(to: T, write: PersistableTransaction) {
    const next = { to };
    setHeld(next);
    const release = () => setHeld((current) => (current === next ? null : current));
    write.isPersisted.promise.then(release, release);
  }

  return [held ? held.to : value, hold] as const;
}
