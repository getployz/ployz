import type { JsonValue } from "@ployz/sdk";
import { Effect, Schema } from "effect";
import { GithubObservationError, type GithubApiService } from "#/modules/github/github-observation.api";

/**
 * GitHub answering JSON by URL: `"down"` fails as unreachable, and an unlisted URL is not found. `calls` records
 * each request's URL and the installation it used, so tests can check which authority read what.
 */
export function fakeGithubApi(answers: Readonly<Record<string, JsonValue>> = {}) {
  const calls: Array<{ readonly url: string; readonly installationId: number | null }> = [];
  const service: GithubApiService = {
    archive: () => Effect.die("no archives in this test"),
    json: (request) => {
      calls.push({ url: request.url, installationId: request.installationId });
      const answer = answers[request.url];
      if (answer === undefined || answer === "down") {
        const code = answer === "down" ? "request_failed" : "not_found";
        return Effect.fail(new GithubObservationError({ code, operation: request.operation, retriable: false, message: code }));
      }
      return Schema.decodeUnknownEffect(request.schema)(answer).pipe(Effect.orDie);
    },
  };
  return { service, calls };
}
