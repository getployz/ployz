import type { JsonValue } from "@ployz/sdk";
import { Effect, Schema } from "effect";
import { GithubObservationError, type GithubApiService } from "#/modules/github/github-observation.api";

type GithubRequest = Parameters<GithubApiService["json"]>[0];

/**
 * GitHub answering JSON by URL, from a table or a function of the request: `"down"` fails as unreachable, and an
 * unlisted URL is not found. `calls` records each request's URL and the installation it used, so tests can check which
 * authority read what.
 */
export function fakeGithubApi(answers: Readonly<Record<string, JsonValue>> = {}) {
  return fakeGithubApiBy((request) => answers[request.url]);
}

/** GitHub answering each request with what `answer` says for it, as `fakeGithubApi` does from a table. */
export function fakeGithubApiBy(answer: (request: GithubRequest) => JsonValue | undefined) {
  const calls: Array<{ readonly url: string; readonly installationId: number | null }> = [];
  const service: GithubApiService = {
    archive: () => Effect.die("no archives in this test"),
    json: (request) => {
      calls.push({ url: request.url, installationId: request.installationId });
      const found = answer(request);
      if (found === undefined || found === "down") {
        const code = found === "down" ? "request_failed" : "not_found";
        return Effect.fail(new GithubObservationError({ code, operation: request.operation, retriable: false, message: code }));
      }
      return Schema.decodeUnknownEffect(request.schema)(found).pipe(Effect.orDie);
    },
  };
  return { service, calls };
}
