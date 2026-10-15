import type { JsonValue } from "@ployz/sdk";
import { Cache, Effect, Layer, Redacted, Schema } from "effect";
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
    exchangeUserCode: () => Effect.die("no user authorizations in this test"),
    userJson: () => Effect.die("no user authorizations in this test"),
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

export type FakeGithubUser = {
  readonly login: string;
  readonly installations: ReadonlyArray<{
    readonly id: number;
    readonly login: string;
    readonly type: "User" | "Organization";
    readonly suspended?: boolean;
  }>;
  /** Organizations this person owns; any other Organization they reach, they only collaborate on. */
  readonly adminOf?: ReadonlyArray<string>;
  /** Organizations this person is a plain member of. */
  readonly memberOf?: ReadonlyArray<string>;
};

/**
 * People authorizing the App, keyed by the OAuth code each hands back. Answers `/user`, paged `/user/installations`,
 * and `/user/memberships/orgs/{org}` with that person's token; `calls` records each path read.
 */
export function fakeGithubUsers(byCode: Readonly<Record<string, FakeGithubUser>>) {
  const calls: string[] = [];
  const tokens = new Map<string, FakeGithubUser>();
  const service: Pick<GithubApiService, "exchangeUserCode" | "userJson"> = {
    exchangeUserCode: (code) => {
      const user = byCode[code];
      if (user === undefined) {
        return Effect.fail(new GithubObservationError({ code: "request_failed", operation: "exchange_user_code", retriable: false, message: "bad code" }));
      }
      const token = `ghu_${code}`;
      tokens.set(token, user);
      return Effect.succeed(Redacted.make(token));
    },
    userJson: (request) => {
      calls.push(request.path);
      const user = tokens.get(Redacted.value(request.token));
      const url = new URL(request.path, "https://api.github.test");
      const answer = ((): JsonValue | undefined => {
        if (user === undefined) return undefined;
        if (url.pathname === "/user") return { login: user.login };
        if (url.pathname === "/user/installations") {
          const perPage = Number(url.searchParams.get("per_page") ?? 30);
          const page = Number(url.searchParams.get("page") ?? 1);
          return {
            installations: user.installations.slice((page - 1) * perPage, page * perPage).map((installation) => ({
              id: installation.id,
              account: { login: installation.login, type: installation.type, avatar_url: null },
              suspended_at: installation.suspended ? "2026-01-01T00:00:00Z" : null,
            })),
          };
        }
        const org = url.pathname.startsWith("/user/memberships/orgs/")
          ? decodeURIComponent(url.pathname.slice("/user/memberships/orgs/".length))
          : null;
        if (org !== null && user.adminOf?.includes(org)) return { state: "active", role: "admin" };
        if (org !== null && user.memberOf?.includes(org)) return { state: "active", role: "member" };
        return undefined;
      })();
      if (answer === undefined) {
        return Effect.fail(new GithubObservationError({ code: "not_found", operation: request.operation, retriable: false, message: "not_found" }));
      }
      return Schema.decodeUnknownEffect(request.schema)(answer).pipe(Effect.orDie);
    },
  };
  return { service, calls };
}
