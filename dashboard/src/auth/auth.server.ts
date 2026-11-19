import "@tanstack/react-start/server-only";
import { getRequest } from "@tanstack/react-start/server";
import { Effect } from "effect";
import { Auth, type AuthSession } from "#/server/auth.server";
export type { AuthSession } from "#/server/auth.server";
import { runActor } from "#/server/tanstack";

export const getAuthSessionEffect = Effect.fn("Auth.getSessionForRequest")(
  (headers: Headers) => Effect.flatMap(Auth, (auth) => auth.getSession(headers)),
);

/** The session of the request being served, shared with every server function inside it. */
export function getAuthSession(): Promise<AuthSession | null> {
  const request = getRequest();
  return runActor(request, getAuthSessionEffect(request.headers));
}

export const signInGithubEffect = Effect.fn("Auth.signInGithub")(
  (headers: Headers, callbackURL: string) =>
    Effect.flatMap(Auth, (auth) => auth.signInGithub(headers, callbackURL)),
);
