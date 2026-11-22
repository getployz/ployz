import type { AuthSession } from "./auth";
import { createAuthClient } from "better-auth/react";
import { deviceAuthorizationClient, inferAdditionalFields } from "better-auth/client/plugins";
import { polarClient } from "@polar-sh/better-auth";

import { sessionAdditionalFields, userAdditionalFields } from "./session-fields";

export const authClient = createAuthClient({
  plugins: [deviceAuthorizationClient(), polarClient(), inferAdditionalFields({ session: sessionAdditionalFields, user: userAdditionalFields })],
});

/** Called by Router hydration before it renders consumers or runs client guards. */
export function initializeAuthSession(initialSession: AuthSession | null) {
  const session = authClient.$store.atoms["session"];
  if (!session) throw new Error("Better Auth session store is unavailable");
  if (session.get().isPending) {
    session.set({ ...session.get(), data: initialSession, error: null, isPending: false });
  }
}

const signOutListeners = new Set<() => void>();

/** Runs `listener` each time this browser signs out, so whatever kept the last person's identity can drop it. */
export function onSignOut(listener: () => void) {
  signOutListeners.add(listener);
  return () => void signOutListeners.delete(listener);
}

export function announceSignOut() {
  for (const listener of signOutListeners) listener();
}
