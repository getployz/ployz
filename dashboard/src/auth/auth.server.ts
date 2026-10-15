import "@tanstack/react-start/server-only";
import { getRequestHeaders } from "@tanstack/react-start/server";
import { Effect } from "effect";
import { Auth, type AuthSession } from "#/server/auth.server";
export type { AuthSession } from "#/server/auth.server";
import { runAppEffect } from "#/server/run.server";

export const getAuthSessionEffect = Effect.fn("Auth.getSessionForRequest")(
  (headers: Headers) => Effect.flatMap(Auth, (auth) => auth.getSession(headers)),
);

export function getAuthSession(
  headers: Headers = getRequestHeaders(),
): Promise<AuthSession | null> {
  return runAppEffect(getAuthSessionEffect(headers));
}

export const signInGithubEffect = Effect.fn("Auth.signInGithub")(
  (headers: Headers, callbackURL: string) =>
    Effect.flatMap(Auth, (auth) => auth.signInGithub(headers, callbackURL)),
);
