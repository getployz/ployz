import { useState } from "react";
import type { PersistableTransaction } from "./collection-field-resources";

export function useCommitHold<T>(value: T) {
  const [held, setHeld] = useState<{ from: T; to: T } | null>(null);
  if (held && held.from !== value) setHeld(null);
  const shown = held && held.from === value ? held.to : value;

  function hold(to: T, write: PersistableTransaction) {
    if (to === value) return;
    const next = { from: value, to };
    setHeld(next);
    const release = () => setHeld((current) => (current === next ? null : current));
    write.isPersisted.promise.then(release, release);
  }

  return [shown, hold] as const;
}
