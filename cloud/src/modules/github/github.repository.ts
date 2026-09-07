import "@tanstack/react-start/server-only";

import { and, eq, lt, sql } from "drizzle-orm";
import { Effect } from "effect";
import {
  githubInstallation as schemaGithubInstallation,
  githubRepositoryCache as schemaGithubRepositoryCache,
} from "#/modules/github/tables";
import { account as schemaAccount } from "#/modules/identity/tables";
import type { GithubRepository } from "#/modules/github/github";
import { Database } from "#/server/database.server";

export const upsertGithubInstallation = Effect.fn(
  "Github.upsertInstallation",
)(function* (data: {
  readonly userId: string;
  readonly installationId: number;
  readonly accountLogin: string;
  readonly accountType: string;
  readonly accountAvatarUrl: string | null;
}) {
  const database = yield* Database;
  const records = yield* database.drizzle
    .insert(schemaGithubInstallation)
    .values(data)
    .onConflictDoUpdate({
      target: [
        schemaGithubInstallation.userId,
        schemaGithubInstallation.installationId,
      ],
      set: {
        accountLogin: data.accountLogin,
        accountType: data.accountType,
        accountAvatarUrl: data.accountAvatarUrl,
      },
    })
    .returning({
      id: schemaGithubInstallation.id,
      userId: schemaGithubInstallation.userId,
      installationId: schemaGithubInstallation.installationId,
      accountLogin: schemaGithubInstallation.accountLogin,
      accountType: schemaGithubInstallation.accountType,
      accountAvatarUrl: schemaGithubInstallation.accountAvatarUrl,
    });
  return records[0] ?? null;
});

export const listGithubInstallationsForUser = Effect.fn(
  "Github.listInstallationsForUser",
)(function* (userId: string) {
  const database = yield* Database;
  return yield* database.drizzle
    .select({
      id: schemaGithubInstallation.id,
      userId: schemaGithubInstallation.userId,
      installationId: schemaGithubInstallation.installationId,
      accountLogin: schemaGithubInstallation.accountLogin,
      accountType: schemaGithubInstallation.accountType,
      accountAvatarUrl: schemaGithubInstallation.accountAvatarUrl,
    })
    .from(schemaGithubInstallation)
    .where(eq(schemaGithubInstallation.userId, userId));
});

export const listGithubInstallationUserIds = Effect.fn(
  "Github.listInstallationUserIds",
)(function* (installationId: number) {
  const database = yield* Database;
  const records = yield* database.drizzle
    .select({ userId: schemaGithubInstallation.userId })
    .from(schemaGithubInstallation)
    .where(eq(schemaGithubInstallation.installationId, installationId));
  return Array.from(new Set(records.map((record) => record.userId)));
});

export const listAllGithubInstallationIds = Effect.fn(
  "Github.listAllInstallationIds",
)(function* () {
  const database = yield* Database;
  const records = yield* database.drizzle
    .select({ installationId: schemaGithubInstallation.installationId })
    .from(schemaGithubInstallation);
  return Array.from(new Set(records.map((record) => record.installationId)));
});

export const deleteGithubInstallationsByProviderId = Effect.fn(
  "Github.deleteInstallationsByProviderId",
)(function* (installationId: number) {
  const database = yield* Database;
  yield* database.drizzle
    .delete(schemaGithubInstallation)
    .where(eq(schemaGithubInstallation.installationId, installationId));
});

export const deleteCachedGithubRepositories = Effect.fn(
  "Github.deleteCachedRepositories",
)(function* (installationId: number) {
  const database = yield* Database;
  yield* database.drizzle
    .delete(schemaGithubRepositoryCache)
    .where(eq(schemaGithubRepositoryCache.installationId, installationId));
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
        sql`${schemaGithubRepositoryCache.repositoryId} = any(${repositoryIds})`,
      ),
    );
});

export const upsertCachedGithubRepositories = Effect.fn(
  "Github.upsertCachedRepositories",
)(function* (data: {
  readonly userIds: readonly string[];
  readonly installationId: number;
  readonly repositories: readonly GithubRepository[];
  readonly syncedAt: Date;
}) {
  if (data.userIds.length === 0 || data.repositories.length === 0) return;
  const database = yield* Database;
  yield* database.drizzle
    .insert(schemaGithubRepositoryCache)
    .values(
      data.userIds.flatMap((userId) =>
        data.repositories.map((repository) => ({
          userId,
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
      ),
    )
    .onConflictDoUpdate({
      target: [
        schemaGithubRepositoryCache.userId,
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

export const getCachedGithubRepositoryForUser = Effect.fn(
  "Github.getCachedRepositoryForUser",
)(function* (input: {
  readonly userId: string;
  readonly installationId: number;
  readonly repositoryId: number;
}) {
  const database = yield* Database;
  const repositories = yield* database.drizzle
    .select({ fullName: schemaGithubRepositoryCache.fullName })
    .from(schemaGithubInstallation)
    .innerJoin(
      schemaGithubRepositoryCache,
      and(
        eq(
          schemaGithubInstallation.installationId,
          schemaGithubRepositoryCache.installationId,
        ),
        eq(
          schemaGithubInstallation.userId,
          schemaGithubRepositoryCache.userId,
        ),
        eq(
          schemaGithubRepositoryCache.installationId,
          input.installationId,
        ),
        eq(schemaGithubRepositoryCache.repositoryId, input.repositoryId),
      ),
    )
    .where(eq(schemaGithubInstallation.userId, input.userId))
    .limit(1);
  return repositories[0] ?? null;
});

export const findUserIdByGithubAccountId = Effect.fn(
  "Github.findUserIdByAccountId",
)(function* (githubAccountId: string) {
  const database = yield* Database;
  const records = yield* database.drizzle
    .select({ userId: schemaAccount.userId })
    .from(schemaAccount)
    .where(
      and(
        eq(schemaAccount.providerId, "github"),
        eq(schemaAccount.accountId, githubAccountId),
      ),
    )
    .limit(1);
  return records[0]?.userId ?? null;
});
