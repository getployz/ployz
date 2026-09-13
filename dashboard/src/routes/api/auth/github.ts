import { createFileRoute } from "@tanstack/react-router";
import { Data, Effect, Schema } from "effect";
import { signInGithubEffect } from "#/auth/auth.server";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

export const Route = createFileRoute("/api/auth/github")({
  server: {
    handlers: {
      POST: async ({ request }) => handleGithubSignIn(request),
    },
  },
});

class GithubOAuthRequestFailure extends Data.TaggedError(
  "GithubOAuthRequestFailure",
)<{ readonly cause: unknown }> {
  readonly publicErrorCategory = "internal" as const;
}

class GithubOAuthFormValidation extends Data.TaggedError(
  "GithubOAuthFormValidation",
)<{ readonly cause: unknown }> {
  readonly publicErrorCategory = "validation" as const;
}

const GithubOAuthForm = Schema.Struct({
  callbackURL: Schema.NullOr(Schema.String),
});

const githubSignInResponse = Effect.fn("Auth.githubSignInResponse")(
  function* (request: Request) {
    const formData = yield* Effect.tryPromise({
      try: () => request.formData(),
      catch: (cause) => new GithubOAuthRequestFailure({ cause }),
    });
    const input = yield* Schema.decodeUnknownEffect(GithubOAuthForm)(
      { callbackURL: formData.get("callbackURL") },
      { onExcessProperty: "error" },
    ).pipe(
      Effect.mapError(
        (cause) => new GithubOAuthFormValidation({ cause }),
      ),
    );
    const callbackURL = safeCallbackURL(input.callbackURL);
    const response = yield* signInGithubEffect(request.headers, callbackURL);
    const location = response.headers.get("location");

    if (!response.ok || !location) {
      return new Response("Could not start GitHub sign-in.", {
        status: response.ok ? 502 : response.status,
        headers: { "cache-control": "no-store" },
      });
    }

    const headers = new Headers(response.headers);
    headers.set("cache-control", "no-store");
    headers.delete("content-type");
    headers.delete("set-cookie");
    headers.set("location", location);
    return new Response(null, { status: 303, headers });
  },
);

export async function handleGithubSignIn(request: Request) {
  try {
    return await runAppEffect(githubSignInResponse(request), {
      signal: request.signal,
    });
  } catch (cause) {
    return publicErrorResponse(cause);
  }
}

function safeCallbackURL(value: string | null) {
  return value !== null && value.startsWith("/") && !value.startsWith("//")
    ? value
    : "/cloud";
}
