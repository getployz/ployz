import "@tanstack/react-start/server-only";
import type { ConfigStore, GitSource } from "@ployz/sdk";
import { and, eq } from "drizzle-orm";
import { Data, Effect } from "effect";
import { GithubDisconnected } from "#/modules/github/github-access";
import { GithubSourceError, resolveGithubSourceSha } from "#/modules/github/github-source.server";
import { account } from "#/modules/identity/tables";
import { member } from "#/modules/organization/tables";
import { refusedWith, storeTry } from "#/modules/config-store/store-sdk.server";
import { Database } from "#/server/database.server";

/** Cloud could not read a Git Service's source; users read the message. */
export class SourceUnreadable extends Data.TaggedError("SourceUnreadable")<{ readonly message: string }> {}

export const fromGithub = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.mapError((error) => new SourceUnreadable({
  message: error instanceof GithubSourceError || error instanceof GithubDisconnected
    ? error.message
    : "GitHub didn't answer while reading the source. Deploy again.",
})));

export const sourceIdentity = (organizationId: string, source: GitSource) => ({
  organizationId,
  installationId: source.access.type === "public" ? null : source.access.installationId,
  repositoryId: source.repository_id,
});

/**
 * The GitHub user ids of the Organization's members who signed in with GitHub: whose pull requests deploy on their own.
 * The Store re-checks a trusted pull request's author against them each time it would deploy its head.
 */
export const githubMembers = Effect.fn("ConfigStore.githubMembers")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select({ accountId: account.accountId }).from(member)
    .innerJoin(account, and(eq(account.userId, member.userId), eq(account.providerId, "github")))
    .where(eq(member.organizationId, organizationId));
  return rows.map((row) => Number(row.accountId)).filter((id) => Number.isSafeInteger(id) && id > 0);
});

/**
 * Pin each Git Service of the Deployment to its branch's head, unless it is pinned already (a pin never moves, so a
 * retried run reads the same commit). Resolves to every source with its pin. A trusted pull request's head pins only
 * while its author is still among the members sent with it, unless a bot opened it from the repository itself.
 */
export const pinStoreSources = Effect.fn("StoreDeployment.pinSources")(function* (
  store: ConfigStore, organizationId: string, deploymentId: string,
) {
  const sources = yield* storeTry(() => store.deploymentSources(deploymentId));
  const heads: Record<string, string> = {};
  // Services on one branch pin one head, read once.
  const resolved = new Map<string, string>();
  for (const source of sources) {
    if (source.commit !== null) continue;
    if (source.branch === null) {
      return yield* new SourceUnreadable({ message: `Reconnect ${source.service}'s branch before deploying.` });
    }
    const branch = { ...sourceIdentity(organizationId, source), branch: source.branch };
    const key = `${branch.installationId}:${branch.repositoryId}:${branch.branch}`;
    const head = resolved.get(key) ?? (yield* fromGithub(resolveGithubSourceSha(branch)));
    resolved.set(key, head);
    heads[source.service] = head;
  }
  if (Object.keys(heads).length === 0) return sources;
  const members = yield* githubMembers(organizationId);
  // A head it can never build, such as one its pull request hasn't approved, is why it failed.
  return yield* storeTry(() => store.pinSources(deploymentId, heads, { members })).pipe(
    Effect.catchIf(refusedWith("invalid_argument"), (refused) => new SourceUnreadable({ message: refused.message })),
  );
});
