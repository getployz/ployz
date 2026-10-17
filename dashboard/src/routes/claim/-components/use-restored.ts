import { useEffect } from "react";

/** Calls `onRestore` when the browser shows this page again from its back-forward cache, frozen as it was left. */
export function useRestored(onRestore: () => void) {
  useEffect(() => {
    const restored = (event: PageTransitionEvent) => {
      if (event.persisted) onRestore();
    };
    window.addEventListener("pageshow", restored);
    return () => window.removeEventListener("pageshow", restored);
  }, [onRestore]);
}
