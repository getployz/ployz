import { createAuthClient } from "better-auth/react";
import { organizationClient } from "better-auth/client/plugins";
import { polarClient } from "@polar-sh/better-auth";

export const authClient = createAuthClient({
  plugins: [organizationClient(), polarClient()],
});

export function initializeAuthSession() {
  const session = authClient.$store.atoms["session"];
  if (!session) throw new Error("Better Auth session store is unavailable");
  return new Promise<void>((resolve) => {
    const unsubscribe = session.subscribe((state) => {
      if (!state.isPending) {
        queueMicrotask(() => {
          unsubscribe();
          resolve();
        });
      }
    });
  });
}
