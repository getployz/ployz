import type { AuthSession } from "./auth";
import { createAuthClient } from "better-auth/react";
import { inferAdditionalFields, organizationClient } from "better-auth/client/plugins";
import { polarClient } from "@polar-sh/better-auth";

import { sessionAdditionalFields, userAdditionalFields } from "./session-fields";

export const authClient = createAuthClient({
  plugins: [organizationClient(), polarClient(), inferAdditionalFields({ session: sessionAdditionalFields, user: userAdditionalFields })],
});

/** Called by Router hydration before it renders consumers or runs client guards. */
export function initializeAuthSession(initialSession: AuthSession | null) {
  const session = authClient.$store.atoms["session"];
  if (!session) throw new Error("Better Auth session store is unavailable");
  if (session.get().isPending) {
    session.set({ ...session.get(), data: initialSession, error: null, isPending: false });
  }
}
