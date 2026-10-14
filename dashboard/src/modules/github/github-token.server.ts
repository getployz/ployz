import "@tanstack/react-start/server-only";
import crypto from "node:crypto";
import { and, eq } from "drizzle-orm";
import { Cache, Context, Effect, Layer, Redacted, Schema } from "effect";
import { GithubDisconnected } from "#/modules/github/github-access";
import { githubTimestampSchema, isValidGithubId } from "#/modules/github/github-ingestion.contracts";
import { GithubObservationError } from "#/modules/github/github-observation.api";
import { githubInstallation, organizationGithubInstallation } from "#/modules/github/tables";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";

export const GITHUB_API_VERSION = "2022-11-28";

/** Installation tokens live for one hour on GitHub. */
const INSTALLATION_TOKEN_TTL = "50 minutes";

export function createGithubAppJwt(input: { readonly appId: string; readonly privateKey: string }): string {
  const privateKey = input.privateKey.replace(/\\n/g, "\n");
  const now = Math.floor(Date.now() / 1000);
  const header = Buffer.from(JSON.stringify({ alg: "RS256", typ: "JWT" })).toString("base64url");
  const payload = Buffer.from(JSON.stringify({ iss: input.appId, iat: now - 60, exp: now + 600 })).toString("base64url");
  const signature = crypto
    .createSign("RSA-SHA256")
    .update(`${header}.${payload}`)
    .sign({ key: privateKey, padding: crypto.constants.RSA_PKCS1_PADDING }, "base64url");
  return `${header}.${payload}.${signature}`;
}

const mintFailed = (args: { code: "request_failed" | "not_found" | "invalid_response"; status?: number; retryAfterSeconds?: number; retriable: boolean }) =>
  new GithubObservationError({ ...args, operation: "installation_token", message: `GitHub installation_token failed (${args.code}).` });

const tokenResponseSchema = Schema.Struct({
  token: Schema.String.check(Schema.isMinLength(1)),
  expires_at: githubTimestampSchema,
});

function retryAfterSeconds(response: Response): number | undefined {
  const value = response.headers.get("retry-after");
  if (!value || !/^\d+$/.test(value)) return undefined;
  const seconds = Number(value);
  return Number.isSafeInteger(seconds) ? seconds : undefined;
}

export const githubAppJwt = Effect.fn("GithubTokens.appJwt")(function* () {
  const { github } = yield* AppConfig;
  return yield* Effect.try({
    try: () => createGithubAppJwt({ appId: github.appId, privateKey: Redacted.value(github.appPrivateKey) }),
    catch: () => mintFailed({ code: "request_failed", retriable: false }),
  });
});

export const mintInstallationToken = Effect.fn("GithubTokens.mint")(function* (installationId: number) {
  const { github } = yield* AppConfig;
  const jwt = yield* githubAppJwt();
  const response = yield* Effect.tryPromise({
    try: (signal) => fetch(`${github.origins.api}/app/installations/${installationId}/access_tokens`, {
      signal,
      method: "POST",
      headers: { Authorization: `Bearer ${jwt}`, Accept: "application/vnd.github+json", "X-GitHub-Api-Version": GITHUB_API_VERSION },
    }),
    catch: () => mintFailed({ code: "request_failed", retriable: true }),
  });
  if (!response.ok) {
    const retriable = response.status === 403 || response.status === 429 || response.status >= 500;
    return yield* mintFailed({
      code: response.status === 404 ? "not_found" : "request_failed",
      status: response.status,
      retryAfterSeconds: retryAfterSeconds(response),
      retriable,
    });
  }
  const body = yield* Effect.tryPromise({
    try: async () => {
      const parsed: unknown = await response.json();
      return parsed;
    },
    catch: () => mintFailed({ code: "request_failed", status: response.status, retriable: true }),
  });
  const decoded = yield* Schema.decodeUnknownEffect(tokenResponseSchema)(body).pipe(
    Effect.mapError(() => mintFailed({ code: "invalid_response", status: response.status, retriable: false })),
  );
  return decoded.token;
});

export interface GithubTokensService {
  /**
   * A token for the Organization's read through `installationId`. The link row is checked on every call, before the
   * cache, so a disconnected Organization fails `GithubDisconnected` even while a token for the installation is cached.
   */
  readonly installationToken: (organizationId: string, installationId: number) => Effect.Effect<string, GithubObservationError | GithubDisconnected>;
  /** A token for Cloud's own bookkeeping of an installation (repository sync, metadata). Never hands repository contents to an Organization. */
  readonly systemInstallationToken: (installationId: number) => Effect.Effect<string, GithubObservationError>;
}

export class GithubTokens extends Context.Service<GithubTokens, GithubTokensService>()("ployz/GithubTokens") {}

export const findInstallationLink = Effect.fn("GithubTokens.findInstallationLink")(function* (organizationId: string, installationId: number) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle
    .select({ accountLogin: githubInstallation.accountLogin, linked: organizationGithubInstallation.organizationId })
    .from(githubInstallation)
    .leftJoin(organizationGithubInstallation, and(
      eq(organizationGithubInstallation.installationId, githubInstallation.installationId),
      eq(organizationGithubInstallation.organizationId, organizationId),
    ))
    .where(eq(githubInstallation.installationId, installationId))
    .limit(1);
  const row = rows[0];
  return { linked: row?.linked !== undefined && row.linked !== null, accountLogin: row?.accountLogin ?? `installation ${installationId}` };
});

export const makeGithubTokens = Effect.fn("GithubTokens.make")(function* (
  mint: (installationId: number) => Effect.Effect<string, GithubObservationError>,
) {
  const database = yield* Database;
  // Concurrent misses for one installation share one mint; a failed mint is evicted so the next caller retries.
  const cache = yield* Cache.make({ capacity: 256, timeToLive: INSTALLATION_TOKEN_TTL, lookup: mint });
  const cached = (installationId: number) => Cache.get(cache, installationId).pipe(
    Effect.tapError(() => Cache.invalidate(cache, installationId)),
  );
  const systemInstallationToken: GithubTokensService["systemInstallationToken"] = Effect.fn("GithubTokens.system")(function* (installationId) {
    if (!isValidGithubId(installationId)) return yield* mintFailed({ code: "request_failed", retriable: false });
    return yield* cached(installationId);
  });
  const installationToken: GithubTokensService["installationToken"] = Effect.fn("GithubTokens.installation")(function* (organizationId, installationId) {
    if (!isValidGithubId(installationId)) return yield* mintFailed({ code: "request_failed", retriable: false });
    const link = yield* findInstallationLink(organizationId, installationId).pipe(Effect.provideService(Database, database), Effect.orDie);
    if (!link.linked) return yield* new GithubDisconnected({ organizationId, installationId, accountLogin: link.accountLogin });
    return yield* cached(installationId);
  });
  return { installationToken, systemInstallationToken } satisfies GithubTokensService;
});

export const GithubTokensLive = Layer.effect(GithubTokens, Effect.gen(function* () {
  const config = yield* AppConfig;
  return yield* makeGithubTokens((installationId) => mintInstallationToken(installationId).pipe(Effect.provideService(AppConfig, config)));
}));
