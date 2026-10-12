import { createFileRoute } from "@tanstack/react-router";
import { Data, Effect } from "effect";
import { getAuthSessionEffect, signInGithubEffect } from "#/auth/auth.server";
import { runAppEffect } from "#/server/run.server";

// Sign in and Deploy on the marketing site link here: one click from the page to GitHub's consent screen.
export const Route = createFileRoute("/auth/github")({
  server: {
    handlers: {
      GET: async ({ request }) => handleGithubSignIn(request),
    },
  },
});

const SIGN_IN_FAILED = "/auth?error=github_unavailable";

class GithubSignInRefused extends Data.TaggedError("GithubSignInRefused")<{
  readonly status: number;
}> {}

const githubSignIn = Effect.fn("Auth.githubSignIn")(function* (request: Request) {
  const callbackURL = safeCallbackURL(new URL(request.url).searchParams.get("callbackURL"));
  const session = yield* getAuthSessionEffect(request.headers);
  if (session !== null) return seeOther(callbackURL);

  // better-auth's OAuth state cookie reaches the browser through tanstackStartCookies, not this response.
  const response = yield* signInGithubEffect(request.headers, callbackURL);
  const location = response.headers.get("location");
  if (!response.ok || location === null) return yield* new GithubSignInRefused({ status: response.status });
  return seeOther(location);
});

export function handleGithubSignIn(request: Request) {
  return runAppEffect(
    githubSignIn(request).pipe(
      Effect.catchCause((cause) =>
        Effect.logError("GitHub sign-in did not start.", cause).pipe(Effect.as(seeOther(SIGN_IN_FAILED))),
      ),
    ),
    { signal: request.signal },
  );
}

function seeOther(location: string) {
  return new Response(null, { status: 303, headers: { location, "cache-control": "no-store" } });
}

function safeCallbackURL(value: string | null) {
  return value !== null && value.startsWith("/") && !value.startsWith("//") ? value : "/cloud";
}
