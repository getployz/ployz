import "@tanstack/react-start/server-only";
import { and, eq } from "drizzle-orm";
import { Cache, Context, Effect, Layer } from "effect";
import { GithubDisconnected, type GithubAccess } from "#/modules/github/github-access";
import { GithubAuthority } from "#/modules/github/github-authority.server";
import { isValidGithubId } from "#/modules/github/github-ingestion.contracts";
import { GithubObservationError } from "#/modules/github/github-observation.api";
import { githubInstallation, organizationGithubInstallation } from "#/modules/github/tables";
import { Database } from "#/server/database.server";

/** Installation tokens live for one hour on GitHub. */
const INSTALLATION_TOKEN_TTL = "50 minutes";

const mintFailed = (args: { code: "request_failed" | "not_found"; retriable: boolean }) =>
  new GithubObservationError({ ...args, operation: "installation_token", message: `GitHub installation_token failed (${args.code}).` });

export interface GithubTokensService {
  /**
   * A token for the Organization's read through `installationId`. The link and the installation are checked on every
   * call, before the cache, so an Organization that disconnected, or whose installation GitHub suspended, fails
   * `GithubDisconnected` even while a token for the installation is cached.
   */
  readonly installationToken: (organizationId: string, installationId: number) => Effect.Effect<string, GithubObservationError | GithubDisconnected>;
  /**
   * A token for Cloud's own acts on an installation: repository sync, a webhook's reads for its linked Organizations,
   * cancelling a run Cloud dispatched. An installation GitHub suspended or Cloud forgot gets none, cached or not.
   */
  readonly systemInstallationToken: (installationId: number) => Effect.Effect<string, GithubObservationError>;
  /**
   * The installation `access` names, when Cloud may act through it now, as the two token reads check: for an
   * Organization, linked and not suspended; for Cloud itself, known and not suspended. Mints nothing.
   */
  readonly usable: (access: Exclude<GithubAccess, { type: "public" }>) => Effect.Effect<number, GithubObservationError | GithubDisconnected>;
}

export class GithubTokens extends Context.Service<GithubTokens, GithubTokensService>()("ployz/GithubTokens") {}

/** What Cloud knows of an installation for one Organization: whether it links it, and whether GitHub suspended it. */
export const findInstallationLink = Effect.fn("GithubTokens.findInstallationLink")(function* (organizationId: string, installationId: number) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle
    .select({
      accountLogin: githubInstallation.accountLogin,
      suspendedAt: githubInstallation.suspendedAt,
      linked: organizationGithubInstallation.organizationId,
    })
    .from(githubInstallation)
    .leftJoin(organizationGithubInstallation, and(
      eq(organizationGithubInstallation.installationId, githubInstallation.installationId),
      eq(organizationGithubInstallation.organizationId, organizationId),
    ))
    .where(eq(githubInstallation.installationId, installationId))
    .limit(1);
  const row = rows[0];
  return {
    installed: row !== undefined,
    linked: row?.linked !== undefined && row.linked !== null,
    suspended: row?.suspendedAt !== undefined && row.suspendedAt !== null,
    accountLogin: row?.accountLogin ?? `installation ${installationId}`,
  };
});

type InstallationLink = Effect.Success<ReturnType<typeof findInstallationLink>>;

const refusalOf = (organizationId: string, installationId: number, link: InstallationLink) => link.linked && !link.suspended
  ? null
  : new GithubDisconnected({ organizationId, installationId, accountLogin: link.accountLogin, reason: link.linked ? "suspended" : "disconnected" });

/** Why the Organization can't use the installation now, or null when it can. */
export const installationRefusal = Effect.fn("GithubTokens.installationRefusal")(function* (organizationId: string, installationId: number) {
  return refusalOf(organizationId, installationId, yield* findInstallationLink(organizationId, installationId));
});

const installationUsable = Effect.fn("GithubTokens.installationUsable")(function* (installationId: number) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select({ suspendedAt: githubInstallation.suspendedAt }).from(githubInstallation)
    .where(eq(githubInstallation.installationId, installationId)).limit(1);
  return rows[0] !== undefined && rows[0].suspendedAt === null;
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
  // Every Cloud process holds its own cache, so a suspended or forgotten installation's token is dropped where it is
  // next asked for, not where the webhook landed.
  const forgotten = (installationId: number) =>
    Cache.invalidate(cache, installationId).pipe(Effect.andThen(mintFailed({ code: "not_found", retriable: false })));
  const systemInstallationToken: GithubTokensService["systemInstallationToken"] = Effect.fn("GithubTokens.system")(function* (installationId) {
    if (!isValidGithubId(installationId)) return yield* mintFailed({ code: "request_failed", retriable: false });
    const usable = yield* installationUsable(installationId).pipe(Effect.provideService(Database, database), Effect.orDie);
    if (!usable) return yield* forgotten(installationId);
    return yield* cached(installationId);
  });
  const installationToken: GithubTokensService["installationToken"] = Effect.fn("GithubTokens.installation")(function* (organizationId, installationId) {
    if (!isValidGithubId(installationId)) return yield* mintFailed({ code: "request_failed", retriable: false });
    const link = yield* findInstallationLink(organizationId, installationId).pipe(Effect.provideService(Database, database), Effect.orDie);
    if (!link.installed || link.suspended) yield* Cache.invalidate(cache, installationId);
    const refusal = refusalOf(organizationId, installationId, link);
    if (refusal !== null) return yield* refusal;
    return yield* cached(installationId);
  });
  const usable: GithubTokensService["usable"] = Effect.fn("GithubTokens.usable")(function* (access) {
    if (!isValidGithubId(access.installationId)) return yield* mintFailed({ code: "request_failed", retriable: false });
    if (access.type === "installation") {
      const known = yield* installationUsable(access.installationId).pipe(Effect.provideService(Database, database), Effect.orDie);
      if (!known) return yield* forgotten(access.installationId);
      return access.installationId;
    }
    const link = yield* findInstallationLink(access.organizationId, access.installationId).pipe(Effect.provideService(Database, database), Effect.orDie);
    if (!link.installed || link.suspended) yield* Cache.invalidate(cache, access.installationId);
    const refusal = refusalOf(access.organizationId, access.installationId, link);
    if (refusal !== null) return yield* refusal;
    return access.installationId;
  });
  return { installationToken, systemInstallationToken, usable } satisfies GithubTokensService;
});

/** Read tokens of the whole read set, one per installation, cached for most of their hour. */
export const GithubTokensLive = Layer.effect(GithubTokens, Effect.gen(function* () {
  const authority = yield* GithubAuthority;
  return yield* makeGithubTokens((installationId) => authority.readToken(installationId));
}));
