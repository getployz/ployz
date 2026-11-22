import "@tanstack/react-start/server-only";
import { getRequest } from "@tanstack/react-start/server";
import { Effect } from "effect";
import { Auth, type AuthSession } from "#/modules/identity/auth.server";
import { runActor } from "#/server/tanstack";

/** The session of the request being served, shared with every server function inside it. */
export function getAuthSession(): Promise<AuthSession | null> {
  const request = getRequest();
  return runActor(request, Effect.flatMap(Auth, (auth) => auth.getSession(request.headers)));
}
