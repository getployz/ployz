import "@tanstack/react-start/server-only";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import { listGithubRepositoryBranches } from "#/modules/github/github.api";
import {
  deleteGithubInstallationForUser,
  listCachedGithubRepositoriesForUser,
  listGithubInstallationsForUser,
} from "#/modules/github/github.repository";
import { getGithubAppInstallUrl } from "#/modules/github/github.server";
import { resolveReadableRepository } from "#/modules/github/readable-repository.server";
import type { Caller } from "#/modules/identity/actor";
import { account } from "#/modules/identity/tables";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";

/**
 * `ployz github` (`GET /api/cli/github`): the install link, whether the caller's GitHub account is linked (GitHub
 * reports an installation by its GitHub user, so an unlinked user's install never completes), and the caller's
 * installations and the repositories they grant. Ready once an installation grants a repository.
 */
export const githubConnection = Effect.fn("GithubCli.connection")(function* (caller: Caller) {
  const config = yield* AppConfig;
  const database = yield* Database;
  const linked = yield* database.drizzle.select({ id: account.id }).from(account)
    .where(and(eq(account.userId, caller.userId), eq(account.providerId, "github"))).limit(1);
  const installations = yield* listGithubInstallationsForUser(caller.userId);
  const repositories = yield* listCachedGithubRepositoriesForUser(caller.userId);
  return {
    install_url: getGithubAppInstallUrl({ slug: config.github.appSlug }),
    linked: linked.length > 0,
    ready: repositories.length > 0,
    installations: installations.map((installation) => ({
      id: installation.installationId,
      account: installation.accountLogin,
      account_type: installation.accountType,
      repositories: repositories.filter((repository) => repository.installationId === installation.installationId).length,
    })),
    repositories: repositories
      .map((repository) => ({
        repository: repository.fullName,
        private: repository.private,
        default_branch: repository.defaultBranch,
        installation: repository.installationId,
      }))
      .sort((left, right) => left.repository.localeCompare(right.repository)),
  };
});

/** `ployz github ls OWNER/REPO`: a repository the Organization may read, and its branches. */
export const githubBranches = Effect.fn("GithubCli.branches")(function* (caller: Caller, repository: string) {
  const readable = yield* resolveReadableRepository(caller.organization.id, repository);
  if (readable === null) {
    return yield* new NotFound({ message: "No repository by that name that this Organization can read." });
  }
  const branches = yield* listGithubRepositoryBranches(readable.installationId, readable.fullName);
  return {
    repository: readable.fullName,
    access: readable.installationId === null ? "public" : "installation",
    default_branch: readable.defaultBranch,
    branches: branches.map((branch) => branch.name),
  };
});

/** `ployz github disconnect ID`: forget one of the caller's installations; uninstalling the App happens on GitHub. */
export const disconnectGithub = Effect.fn("GithubCli.disconnect")(function* (caller: Caller, installationId: number) {
  const removed = yield* deleteGithubInstallationForUser({ userId: caller.userId, installationId });
  if (removed === null) return yield* new NotFound({ message: "No such GitHub installation of yours." });
  const settings = removed.accountType === "Organization"
    ? `https://github.com/organizations/${removed.accountLogin}/settings/installations/${removed.installationId}`
    : `https://github.com/settings/installations/${removed.installationId}`;
  return { disconnected: { id: removed.installationId, account: removed.accountLogin }, uninstall_url: settings };
});
