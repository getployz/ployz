import "@tanstack/react-start/server-only";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import { listGithubRepositoryBranches, listGithubRepositoryFiles, readGithubRepositoryContents } from "#/modules/github/github.api";
import { isGithubObservationNotFound } from "#/modules/github/github-observation.api";
import {
  deleteGithubInstallationForUser,
  listCachedGithubRepositoriesForUser,
  listGithubInstallationsForUser,
} from "#/modules/github/github.repository";
import { basicGlob, getGithubAppInstallUrl } from "#/modules/github/github.server";
import { resolveReadableRepository, type ReadableRepository } from "#/modules/github/readable-repository.server";
import type { Caller } from "#/modules/identity/actor";
import { account } from "#/modules/identity/tables";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { NotFound, Validation } from "#/server/public-error";

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

/** A repository the caller's Organization may read; a private one it can't is as missing as one that doesn't exist. */
const readableBy = Effect.fn("GithubCli.readableBy")(function* (caller: Caller, repository: string) {
  const readable = yield* resolveReadableRepository(caller.organization.id, repository);
  if (readable === null) {
    return yield* new NotFound({ message: "No repository by that name that this Organization can read." });
  }
  return readable;
});

/** `ployz github ls OWNER/REPO`: a repository the Organization may read, and its branches. */
export const githubBranches = Effect.fn("GithubCli.branches")(function* (caller: Caller, repository: string) {
  const readable = yield* readableBy(caller, repository);
  const branches = yield* listGithubRepositoryBranches(readable.installationId, readable.fullName);
  return {
    repository: readable.fullName,
    access: readable.installationId === null ? "public" : "installation",
    default_branch: readable.defaultBranch,
    branches: branches.map((branch) => branch.name),
  };
});

/** The most paths `github tree` lists; name a directory or a narrower glob for the rest. */
export const TREE_PATHS = 500;
/** The largest file `github cat` returns the text of. */
export const FILE_BYTES = 64 * 1024;

const directoryOf = (path: string | null) => (path ?? "").replace(/^(\.?\/)+|\/+$/g, "");

export type TreeQuery = {
  /** A directory: only files under it. */
  readonly path?: string | null;
  /** A branch, tag or commit; the default branch if absent. */
  readonly ref?: string | null;
  /** A glob the whole repository-relative path must match. */
  readonly match?: string | null;
};

/** The files of `repository` that `query` selects, at most `TREE_PATHS`. */
export const repositoryTree = Effect.fn("GithubCli.repositoryTree")(function* (
  repository: ReadableRepository, { path = null, ref = null, match = null }: TreeQuery,
) {
  const at = ref ?? repository.defaultBranch;
  const listed = yield* listGithubRepositoryFiles(repository.installationId, repository.fullName, at).pipe(
    Effect.catchIf(isGithubObservationNotFound, () => new NotFound({ message: `No ref ${at} in ${repository.fullName}.` })));
  const directory = directoryOf(path);
  const under = directory === "" ? listed.paths : listed.paths.filter((file) => file.startsWith(`${directory}/`));
  const matcher = match === null ? null : basicGlob(match);
  const matching = matcher === null ? under : under.filter((file) => matcher.match(file));
  return {
    repository: repository.fullName,
    ref: at,
    paths: matching.slice(0, TREE_PATHS),
    truncated: listed.truncated || matching.length > TREE_PATHS,
  };
});

const text = new TextDecoder("utf-8", { fatal: true });

/** The text of one file of `repository` at `ref` (its default branch if null); null with a note past `FILE_BYTES` or for binary. */
export const repositoryFile = Effect.fn("GithubCli.repositoryFile")(function* (
  repository: ReadableRepository, path: string, ref: string | null,
) {
  const at = ref ?? repository.defaultBranch;
  const file = directoryOf(path);
  if (file === "") return yield* new Validation({ message: "Name a file; list the repository with github tree.", userFacing: true });
  const contents = yield* readGithubRepositoryContents(repository.installationId, repository.fullName, file, at).pipe(
    Effect.catchIf(isGithubObservationNotFound,
      () => new NotFound({ message: `No file ${file} at ${at} in ${repository.fullName}.` })));
  if (!("type" in contents)) {
    return yield* new Validation({ message: `${file} is a directory: list it with github tree.`, userFacing: true });
  }
  const read = { repository: repository.fullName, ref: at, path: file, size: contents.size };
  if (contents.type !== "file") return { ...read, content: null, note: `${file} is a ${contents.type}, not a file.` };
  if (contents.size > FILE_BYTES || contents.encoding !== "base64" || contents.content === undefined) {
    return { ...read, content: null, note: `${file} is ${contents.size} bytes, over the ${FILE_BYTES / 1024} KiB github cat reads.` };
  }
  const bytes = Buffer.from(contents.content, "base64");
  const decoded = bytes.includes(0) ? null : decode(bytes);
  return decoded === null
    ? { ...read, content: null, note: `${file} is binary; github cat reads text only.` }
    : { ...read, content: decoded };
});

function decode(bytes: Uint8Array) {
  try {
    return text.decode(bytes);
  } catch {
    return null;
  }
}

export const githubTree = Effect.fn("GithubCli.tree")(function* (caller: Caller, repository: string, query: TreeQuery) {
  return yield* repositoryTree(yield* readableBy(caller, repository), query);
});

/** `ployz github cat OWNER/REPO PATH`: one file of a readable repository. */
export const githubFile = Effect.fn("GithubCli.file")(function* (
  caller: Caller, repository: string, path: string, ref: string | null,
) {
  return yield* repositoryFile(yield* readableBy(caller, repository), path, ref);
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
