/**
 * The Source step a tap signs: the Environment's automatic deploy rule, drafted in this browser with the ployz-core code
 * the Servers check, from the Store's staged rule view, as the last step of the bundle a Save or a Deploy signs.
 */
import type { AutoDeployRule, RuleDraft, RuleView, SavedRule, VersionsDraft } from "@ployz/sdk";
import { type PushBranch, type SourceAsked, versionSteps } from "@ployz/sdk/authority";
import type { QueryClient } from "@tanstack/react-query";
import { githubRepositoryOwnerQueryOptions } from "#/modules/github/github.queries";

/** The repository owner's GitHub ID, or a reason the rule can't name it. */
export type OwnerOf = (repository: string, repositoryId: number) => Promise<{ ownerId: number } | { problem: string }>;

const DRAFT = "This browser couldn't draft the rule.";

const parsed = (body: string): AutoDeployRule => {
  // SAFETY: ployz-core drafted or the Store kept this body as an `AutoDeployRule`.
  return JSON.parse(body) as AutoDeployRule;
};

/** What a tap asks `versionSteps` for, besides the Source step. */
export type Asked = Omit<Parameters<typeof versionSteps>[0], "source">;

/** A bundle drafted with the Source step its view needs, or why the rule can't be signed and whether it was drafted. */
export type SourceDrafted =
  | { readonly kind: "drafted"; readonly draft: VersionsDraft | null }
  | { readonly kind: "held"; readonly reason: string; readonly drafted: boolean };

/**
 * `asked`'s bundle with the Source step `view` needs for the Cluster's `members`, drafted once to learn the repository
 * and again with its owner's GitHub ID, which every GitHub token the rule takes must name. `on` turns the rule on for
 * that branch.
 */
export async function draftSource(asked: Asked, view: RuleView, members: readonly string[], ownerOf: OwnerOf, on?: PushBranch): Promise<SourceDrafted> {
  const draft = (ownerId: number) => {
    const source: SourceAsked = { rule: view.rule ?? null, sources: view.sources, target: view.target, machines: [...members], owner_id: ownerId };
    if (on !== undefined) source.on = on;
    return versionSteps({ ...asked, source });
  };
  let unowned: VersionsDraft | null;
  try {
    unowned = draft(0);
  } catch (error) {
    return { kind: "held", reason: error instanceof Error && error.message !== "" ? error.message : DRAFT, drafted: false };
  }
  if (unowned?.source === undefined) return { kind: "drafted", draft: unowned };
  try {
    const { repository, repository_id } = parsed(unowned.source.body).source;
    const owner = await ownerOf(repository, repository_id);
    if ("problem" in owner) return { kind: "held", reason: owner.problem, drafted: true };
    return { kind: "drafted", draft: draft(owner.ownerId) };
  } catch (error) {
    return { kind: "held", reason: error instanceof Error && error.message !== "" ? error.message : DRAFT, drafted: true };
  }
}

/** The rule `view` keeps, as a Save or Deploy that signs another names it: the Store refuses either if it moved. */
export function priorOf(view: RuleView): SavedRule["prior"] {
  const kept = view.rule === null || view.rule === undefined ? null : parsed(view.rule.body);
  return kept === null ? null : { rule: kept.rule, generation: kept.generation };
}

/** The branch whose pushes stop deploying when `drafted` moves the rule `view` keeps on to another branch. */
export function movedFrom(view: RuleView, drafted: RuleDraft): string | null {
  const kept = view.rule === null || view.rule === undefined ? null : parsed(view.rule.body);
  if (kept === null || kept.ended) return null;
  const next = parsed(drafted.body).source;
  return kept.source.repository_id === next.repository_id && kept.source.branch === next.branch ? null : kept.source.branch;
}

/** The repository owner's GitHub ID as this organization's GitHub connection reads it. */
export const ownerLookup = (queryClient: QueryClient, organizationSlug: string): OwnerOf => async (repository, repositoryId) => {
  try {
    const owner = await queryClient.fetchQuery(githubRepositoryOwnerQueryOptions(organizationSlug, repository));
    return owner.repositoryId === repositoryId ? { ownerId: owner.ownerId }
      : { problem: `${repository} now names another repository. Connect your services to it again.` };
  } catch {
    return { problem: `Couldn't read ${repository} on GitHub.` };
  }
};
