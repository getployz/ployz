import type { JsonValue } from "@ployz/sdk";
import { Cache, Effect, Layer, Schema } from "effect";
import type { GithubAccess } from "#/modules/github/github-access";
import { GithubObservationError, type GithubApiService } from "#/modules/github/github-observation.api";
import { GithubTokens, mintInstallationToken } from "#/modules/github/github-token.server";
import { AppConfig } from "#/server/config.server";

type GithubRequest = Parameters<GithubApiService["json"]>[0];

export const GithubTokensUnchecked = Layer.effect(GithubTokens, Effect.gen(function* () {
  const config = yield* AppConfig;
  const cache = yield* Cache.make({
    capacity: 64,
    lookup: (installationId: number) => mintInstallationToken(installationId).pipe(Effect.provideService(AppConfig, config)),
  });
  const token = (installationId: number) => Cache.get(cache, installationId);
  return { installationToken: (_organizationId, installationId) => token(installationId), systemInstallationToken: token };
}));

/**
 * GitHub answering JSON by API path, from a table or a function of the request: `"down"` fails as unreachable, and an
 * unlisted path is not found. `calls` records each request's path and the access it used, so tests can check which
 * Organization read what.
 */
export function fakeGithubApi(answers: Readonly<Record<string, JsonValue>> = {}) {
  return fakeGithubApiBy((request) => answers[request.path]);
}

/** GitHub answering each request with what `answer` says for it, as `fakeGithubApi` does from a table. */
export function fakeGithubApiBy(answer: (request: GithubRequest) => JsonValue | undefined) {
  const calls: Array<{ readonly path: string; readonly access: GithubAccess }> = [];
  const service: GithubApiService = {
    archive: () => Effect.die("no archives in this test"),
    app: () => Effect.die("no app reads in this test"),
    installationRepositories: () => Effect.die("no installation listings in this test"),
    userInstallations: () => Effect.die("no user authorizations in this test"),
    json: (request) => {
      calls.push({ path: request.path, access: request.access });
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
