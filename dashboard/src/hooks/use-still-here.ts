import { useRouter } from "@tanstack/react-router";

/**
 * Call when an action starts; the returned check says whether the user is still on that location when it finishes.
 * Judged by the location key, not unmount: a pending navigation keeps the component mounted, and A→B→A remounts it.
 */
export function useStillHere() {
  const router = useRouter();
  return () => {
    const key = router.state.location.state.key;
    return () => router.state.location.state.key === key;
  };
}
