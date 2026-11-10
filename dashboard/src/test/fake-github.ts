import type { JsonValue } from "@ployz/sdk";
import { Cache, Effect, Layer, Schema } from "effect";
import type { GithubAccess } from "#/modules/github/github-access";
import { GithubAuthority, GithubAuthorityLive, type GithubAuthorityService, type GithubConnectedUser } from "#/modules/github/github-authority.server";
import { GithubObservationError, type GithubApiService } from "#/modules/github/github-observation.api";
import { GithubTokens } from "#/modules/github/github-token.server";

type GithubRequest = Parameters<GithubApiService["json"]>[0];

/** Read tokens from the real authority with no link checks, for tests of what GitHub answers. */
export const GithubTokensUnchecked = Layer.effect(GithubTokens, Effect.gen(function* () {
  const authority = yield* GithubAuthority;
  const cache = yield* Cache.make({ capacity: 64, lookup: (installationId: number) => authority.readToken(installationId) });
  const token = (installationId: number) => Cache.get(cache, installationId);
  return {
    installationToken: (_organizationId, installationId) => token(installationId),
    systemInstallationToken: token,
    usable: (access) => Effect.succeed(access.installationId),
  };
})).pipe(Layer.provideMerge(GithubAuthorityLive));

/** One operation a test's authority was asked for, with its first argument. */
export type GithubAuthorityCall = {
  [K in keyof GithubAuthorityService]: { readonly operation: K; readonly input: Parameters<GithubAuthorityService[K]>[0] };
}[keyof GithubAuthorityService];

/**
 * The App's authority answering each operation with what `answers` gives for it. `readToken` hands out a fake token;
 * any other operation a test didn't answer fails it. `calls` records each operation and its first argument.
 */
export function fakeGithubAuthority(answers: Partial<GithubAuthorityService> = {}) {
  const calls: GithubAuthorityCall[] = [];
  const unanswered = (operation: keyof GithubAuthorityService) => Effect.die(`no ${operation} in this test`);
  const service: GithubAuthorityService = {
    readToken: (installationId, scope) => {
      calls.push({ operation: "readToken", input: installationId });
      return answers.readToken?.(installationId, scope) ?? Effect.succeed(`fake-read-token-${installationId}`);
    },
    installation: (input) => {
      calls.push({ operation: "installation", input });
      return answers.installation?.(input) ?? unanswered("installation");
    },
    buildWorkflow: (input) => {
      calls.push({ operation: "buildWorkflow", input });
      return answers.buildWorkflow?.(input) ?? unanswered("buildWorkflow");
    },
    buildRuns: (input) => {
      calls.push({ operation: "buildRuns", input });
      return answers.buildRuns?.(input) ?? unanswered("buildRuns");
    },
    buildRun: (input) => {
      calls.push({ operation: "buildRun", input });
      return answers.buildRun?.(input) ?? unanswered("buildRun");
    },
    dispatchBuild: (input) => {
      calls.push({ operation: "dispatchBuild", input });
      return answers.dispatchBuild?.(input) ?? unanswered("dispatchBuild");
    },
    dispatchApprovedBuild: (input) => {
      calls.push({ operation: "dispatchApprovedBuild", input });
      return answers.dispatchApprovedBuild?.(input) ?? unanswered("dispatchApprovedBuild");
    },
    cancelBuild: (input) => {
      calls.push({ operation: "cancelBuild", input });
      return answers.cancelBuild?.(input) ?? unanswered("cancelBuild");
    },
    rerunBuild: (input) => {
      calls.push({ operation: "rerunBuild", input });
      return answers.rerunBuild?.(input) ?? unanswered("rerunBuild");
    },
    postCheck: (input) => {
      calls.push({ operation: "postCheck", input });
      return answers.postCheck?.(input) ?? unanswered("postCheck");
    },
    connect: (input) => {
      calls.push({ operation: "connect", input });
      return answers.connect?.(input) ?? unanswered("connect");
    },
  };
  return { service, calls };
}

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
    installationOf: (access) => access.type === "public"
      ? Effect.fail(new GithubObservationError({ code: "invalid_argument", operation: "installation_token", retriable: false, message: "public" }))
      : Effect.succeed(access.installationId),
    installationRepositories: () => Effect.die("no installation listings in this test"),
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
 * People authorizing the App, keyed by the OAuth code each hands back: what the authority's connect answers for that
 * code. An Organization's admin administers it; a User administers only their own account.
 */
export function fakeGithubUsers(byCode: Readonly<Record<string, FakeGithubUser>>) {
  const connect: GithubAuthorityService["connect"] = (code) => {
    const user = Object.hasOwn(byCode, code) ? byCode[code] : undefined;
    if (user === undefined) {
      return Effect.fail(new GithubObservationError({ code: "request_failed", operation: "exchange_user_code", retriable: false, message: "bad code" }));
    }
    return Effect.succeed({
      login: user.login,
      installations: user.installations.map((installation) => ({
        id: installation.id,
        account: { login: installation.login, type: installation.type, avatarUrl: null },
        suspendedAt: installation.suspended ? "2026-01-01T00:00:00Z" : null,
        administers: installation.type === "Organization"
          ? user.adminOf?.includes(installation.login) ?? false
          : installation.login.toLowerCase() === user.login.toLowerCase(),
      })),
    } satisfies GithubConnectedUser);
  };
  return { connect };
}
