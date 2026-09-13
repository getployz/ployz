import { createIsomorphicFn } from "@tanstack/react-start";
import { authClient } from "#/auth/auth-client";

export type AuthSession =
  Awaited<ReturnType<typeof import("#/auth/auth.server").getAuthSession>>;

export const getAuthSession = createIsomorphicFn()
  .client(async (): Promise<AuthSession | null> => {
    const sessionAtom = authClient.$store.atoms["session"];
    if (!sessionAtom) return null;
    const state = sessionAtom.get();
    if (state.error) throw state.error;

    return (
      // SAFETY: better-auth's client session atom is untyped relative to the server AuthSession shape; isomorphic getAuthSession keeps them aligned.
      (state.data as AuthSession | null) ?? null
    );
  })
  .server(async (): Promise<AuthSession | null> => {
    const { getAuthSession: getAuthSessionServer } = await import(
      "#/auth/auth.server"
    );

    return getAuthSessionServer();
  });
