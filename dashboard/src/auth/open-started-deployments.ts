import { toast } from "sonner";
import { authClient } from "./auth-client";

const sessionAtom = () => authClient.$store.atoms["session"];

/**
 * The per-user "open deployments I start" preference, stored on the Better Auth user.
 * Read at call time from the session store, which Better Auth refetches after `update-user`.
 */
export function openStartedDeployments() {
  return sessionAtom()?.get().data?.user.openStartedDeployments ?? true;
}

function setInSession(value: boolean) {
  const session = sessionAtom();
  const current = session?.get();
  if (!session || !current?.data) return;
  session.set({ ...current, data: { ...current.data, user: { ...current.data.user, openStartedDeployments: value } } });
}

/** Applies the preference in memory at once, saves it in the background, and rolls back if the save fails. */
export async function setOpenStartedDeployments(value: boolean) {
  if (openStartedDeployments() === value) return;
  setInSession(value);
  const result = await authClient.updateUser({ openStartedDeployments: value });
  if (!result.error) return;
  setInSession(!value);
  toast.error("Couldn't save your deployment preference.");
}

/**
 * How watching a deployment changes the preference: opening your own running attempt turns it on,
 * and returning to live while it runs turns it off. Null leaves it alone.
 */
export function openStartedDeploymentsChange({ shownBefore, shownNow, deploymentId }: {
  /** Your own running attempt shown before and now, if any. */
  shownBefore: string | null; shownNow: string | null;
  /** The attempt in the URL; null is Live Mode. */
  deploymentId: string | null;
}): boolean | null {
  if (shownNow !== null && shownNow !== shownBefore) return true;
  if (shownBefore !== null && deploymentId === null) return false;
  return null;
}
