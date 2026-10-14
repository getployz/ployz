import "@tanstack/react-start/server-only";

import { and, eq, inArray, lt, sql } from "drizzle-orm";
import { Effect } from "effect";
import {
  githubInstallation as schemaGithubInstallation,
  githubRepositoryCache as schemaGithubRepositoryCache,
  organizationGithubInstallation as schemaLink,
} from "#/modules/github/tables";
import type { GithubRepository } from "#/modules/github/github";
import { Database } from "#/server/database.server";

/** What GitHub told us about an installation. Recording it grants nothing; only a link row does. */
export const upsertGithubInstallation = Effect.fn(
  "Github.upsertInstallation",
)(function* (data: {
  readonly installationId: number;
  readonly accountLogin: string;
  readonly accountType: string;
  readonly accountAvatarUrl: string | null;
}) {
  const database = yield* Database;
  yield* database.drizzle
    .insert(schemaGithubInstallation)
    .values(data)
    .onConflictDoUpdate({
      target: schemaGithubInstallation.installationId,
      set: {
        accountLogin: data.accountLogin,
        accountType: data.accountType,
        accountAvatarUrl: data.accountAvatarUrl,
      },
    });
});

const linkedInstallation = {
  installationId: schemaGithubInstallation.installationId,
  accountLogin: schemaGithubInstallation.accountLogin,
  accountType: schemaGithubInstallation.accountType,
  accountAvatarUrl: schemaGithubInstallation.accountAvatarUrl,
  connectedGithubLogin: schemaLink.connectedGithubLogin,
  connectedAt: schemaLink.connectedAt,
};

/** The installations the Organization reads through, with who connected each. */
export const listGithubInstallationsForOrganization = Effect.fn(
  "Github.listInstallationsForOrganization",
)(function* (organizationId: string) {
  const database = yield* Database;
  return yield* database.drizzle
    .select(linkedInstallation)
    .from(schemaLink)
    .innerJoin(schemaGithubInstallation, eq(schemaGithubInstallation.installationId, schemaLink.installationId))
    .where(eq(schemaLink.organizationId, organizationId))
    .orderBy(schemaGithubInstallation.accountLogin, schemaGithubInstallation.installationId);
});

/** Every Organization linked to the installation, in a fixed order so fan-out reads GitHub once under the first. */
export const listLinkedOrganizationIds = Effect.fn("Github.listLinkedOrganizationIds")(
  function* (installationId: number) {
    const database = yield* Database;
    const rows = yield* database.drizzle.select({ organizationId: schemaLink.organizationId })
      .from(schemaLink)
      .where(eq(schemaLink.installationId, installationId))
      .orderBy(schemaLink.connectedAt, schemaLink.organizationId);
    return rows.map((row) => row.organizationId);
  },
);

export const linkGithubInstallation = Effect.fn("Github.linkInstallation")(
  function* (data: {
    readonly organizationId: string;
    readonly installationId: number;
    readonly connectedByUserId: string | null;
    readonly connectedGithubLogin: string;
  }) {
    const database = yield* Database;
    yield* database.drizzle.insert(schemaLink).values(data).onConflictDoNothing();
  },
);

/** Forget the Organization's link only; the installation, its cache and other Organizations' links stay. */
export const unlinkGithubInstallation = Effect.fn("Github.unlinkInstallation")(
  function* (input: { readonly organizationId: string; readonly installationId: number }) {
    const database = yield* Database;
    const removed = yield* database.drizzle.delete(schemaLink)
      .where(and(eq(schemaLink.organizationId, input.organizationId), eq(schemaLink.installationId, input.installationId)))
      .returning({ installationId: schemaLink.installationId });
    return removed.length > 0;
  },
);

export const getGithubInstallation = Effect.fn("Github.getInstallation")(
  function* (installationId: number) {
    const database = yield* Database;
    const rows = yield* database.drizzle.select({
      installationId: schemaGithubInstallation.installationId,
      accountLogin: schemaGithubInstallation.accountLogin,
      accountType: schemaGithubInstallation.accountType,
    }).from(schemaGithubInstallation).where(eq(schemaGithubInstallation.installationId, installationId)).limit(1);
    return rows[0] ?? null;
  },
);

export const listAllGithubInstallationIds = Effect.fn(
  "Github.listAllInstallationIds",
)(function* () {
  const database = yield* Database;
  const records = yield* database.drizzle
    .select({ installationId: schemaGithubInstallation.installationId })
    .from(schemaGithubInstallation);
  return records.map((record) => record.installationId);
});

/** Forget the installation. Its repository cache and every Organization's link cascade with it. */
export const deleteGithubInstallation = Effect.fn(
  "Github.deleteInstallation",
)(function* (installationId: number) {
  const database = yield* Database;
  yield* database.drizzle
    .delete(schemaGithubInstallation)
    .where(eq(schemaGithubInstallation.installationId, installationId));
});

export const deleteCachedGithubRepositoriesById = Effect.fn(
  "Github.deleteCachedRepositoriesById",
)(function* (installationId: number, repositoryIds: readonly number[]) {
  if (repositoryIds.length === 0) return;
  const database = yield* Database;
  yield* database.drizzle
    .delete(schemaGithubRepositoryCache)
    .where(
      and(
        eq(schemaGithubRepositoryCache.installationId, installationId),
        inArray(schemaGithubRepositoryCache.repositoryId, repositoryIds),
      ),
    );
});

export const upsertCachedGithubRepositories = Effect.fn(
  "Github.upsertCachedRepositories",
)(function* (data: {
  readonly installationId: number;
  readonly repositories: readonly GithubRepository[];
  readonly syncedAt: Date;
}) {
  if (data.repositories.length === 0) return;
  const database = yield* Database;
  yield* database.drizzle
    .insert(schemaGithubRepositoryCache)
    .values(
      data.repositories.map((repository) => ({
        installationId: data.installationId,
        repositoryId: repository.id,
        name: repository.name,
        fullName: repository.full_name,
        defaultBranch: repository.default_branch,
        private: repository.private,
        htmlUrl: repository.html_url,
        repoUpdatedAt: new Date(repository.repo_updated_at),
        syncedAt: data.syncedAt,
      })),
    )
    .onConflictDoUpdate({
      target: [
        schemaGithubRepositoryCache.installationId,
        schemaGithubRepositoryCache.repositoryId,
      ],
      set: {
        name: sql`excluded.name`,
        fullName: sql`excluded.full_name`,
        defaultBranch: sql`excluded.default_branch`,
        private: sql`excluded.private`,
        htmlUrl: sql`excluded.html_url`,
        repoUpdatedAt: sql`excluded.repo_updated_at`,
        syncedAt: sql`excluded.synced_at`,
      },
    });
});

export const pruneCachedGithubRepositories = Effect.fn(
  "Github.pruneCachedRepositories",
)(function* (installationId: number, syncedAt: Date) {
  const database = yield* Database;
  const deleted = yield* database.drizzle
    .delete(schemaGithubRepositoryCache)
    .where(
      and(
        eq(schemaGithubRepositoryCache.installationId, installationId),
        lt(schemaGithubRepositoryCache.syncedAt, syncedAt),
      ),
    )
    .returning({ repositoryId: schemaGithubRepositoryCache.repositoryId });
  return deleted.length;
});

const cachedRepository = {
  installationId: schemaGithubRepositoryCache.installationId,
  repositoryId: schemaGithubRepositoryCache.repositoryId,
  name: schemaGithubRepositoryCache.name,
  fullName: schemaGithubRepositoryCache.fullName,
  defaultBranch: schemaGithubRepositoryCache.defaultBranch,
  private: schemaGithubRepositoryCache.private,
  htmlUrl: schemaGithubRepositoryCache.htmlUrl,
  repoUpdatedAt: schemaGithubRepositoryCache.repoUpdatedAt,
  syncedAt: schemaGithubRepositoryCache.syncedAt,
};

const linkedCache = (organizationId: string) =>
  and(
    eq(schemaLink.organizationId, organizationId),
    eq(schemaLink.installationId, schemaGithubRepositoryCache.installationId),
  );

/** The repositories the Organization's linked installations grant, which a repository picker reads. */
export const listCachedGithubRepositoriesForOrganization = Effect.fn(
  "Github.listCachedRepositoriesForOrganization",
)(function* (organizationId: string) {
  const database = yield* Database;
  return yield* database.drizzle.select(cachedRepository)
    .from(schemaGithubRepositoryCache)
    .innerJoin(schemaLink, linkedCache(organizationId))
    .orderBy(schemaGithubRepositoryCache.fullName, schemaGithubRepositoryCache.installationId);
});

export const getCachedGithubRepositoryForOrganization = Effect.fn("Github.getCachedRepositoryForOrganization")(
  function* (input: { organizationId: string; installationId: number; repositoryId: number }) {
    const database = yield* Database;
    const rows = yield* database.drizzle.select({ fullName: schemaGithubRepositoryCache.fullName })
      .from(schemaGithubRepositoryCache)
      .innerJoin(schemaLink, linkedCache(input.organizationId))
      .where(and(
        eq(schemaGithubRepositoryCache.installationId, input.installationId),
        eq(schemaGithubRepositoryCache.repositoryId, input.repositoryId),
      ))
      .limit(1);
    return rows[0] ?? null;
  },
);

/** A repository the Organization reads through a linked installation, by `owner/name` in any case. */
export const findGithubRepositoryByNameForOrganization = Effect.fn("Github.findRepositoryByNameForOrganization")(
  function* (input: { organizationId: string; fullName: string }) {
    const database = yield* Database;
    const rows = yield* database.drizzle.select({
      fullName: schemaGithubRepositoryCache.fullName,
      repositoryId: schemaGithubRepositoryCache.repositoryId,
      installationId: schemaGithubRepositoryCache.installationId,
      defaultBranch: schemaGithubRepositoryCache.defaultBranch,
    })
      .from(schemaGithubRepositoryCache)
      .innerJoin(schemaLink, linkedCache(input.organizationId))
      .where(sql`lower(${schemaGithubRepositoryCache.fullName}) = lower(${input.fullName})`)
      .orderBy(schemaGithubRepositoryCache.installationId)
      .limit(1);
    return rows[0] ?? null;
  },
);

/** The first `limit` repositories, by name, that the Organization's linked installations grant. */
export const listGithubRepositoryNamesForOrganization = Effect.fn("Github.listRepositoryNamesForOrganization")(
  function* (input: { organizationId: string; limit: number }) {
    const database = yield* Database;
    const rows = yield* database.drizzle.selectDistinct({ fullName: schemaGithubRepositoryCache.fullName })
      .from(schemaGithubRepositoryCache)
      .innerJoin(schemaLink, linkedCache(input.organizationId))
      .orderBy(schemaGithubRepositoryCache.fullName)
      .limit(input.limit);
    return rows.map((row) => row.fullName);
  },
);
