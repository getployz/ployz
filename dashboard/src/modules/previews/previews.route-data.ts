import type { PrPlan } from "@ployz/sdk";
import { notFound } from "@tanstack/react-router";
import { prefetchRemote, scopeOf, streamRemote, type RouteDataContext } from "#/collections/route-data";
import { prPlansQuery } from "#/modules/config-store/store-pull-requests";
import { requireStoreProjects } from "#/modules/config-store/store.route-data";
import { environmentsQuery, requireView, storeViewOptions } from "#/modules/config-store/store-view.queries";
import type { PreviewRuleQuery, PreviewsQuery } from "./previews.contract";
import { previewRuleQueryOptions, previewsQueryOptions } from "./previews.queries";

type Place = { organizationSlug: string; projectSlug: string; environmentSlug: string };

/** The Project's PR plans: one per repository its Services deploy from. */
async function plansOf(context: RouteDataContext, place: Pick<Place, "organizationSlug" | "projectSlug">) {
  return requireView(await context.queryClient.ensureQueryData(
    storeViewOptions(place.organizationSlug, scopeOf(context), prPlansQuery(place.projectSlug)))).plans;
}

const repositoryOf = (plan: PrPlan) => ({ repository: plan.repository, repositoryId: plan.repository_id, startFrom: plan.start_from });

/** The repository a previews page names by its ID; not found when no Service of the Project deploys from it. */
export async function requirePreviewRepository(context: RouteDataContext, place: Place, repositoryId: string) {
  const plan = (await plansOf(context, place)).find((candidate) => String(candidate.repository_id) === repositoryId);
  if (plan === undefined) throw notFound();
  return repositoryOf(plan);
}

/**
 * The repository Pull requests opens on from the rail: the one that starts from this Environment, else the Project's
 * first. Null when no Service of the Project deploys from a repository.
 */
export async function requireDefaultPreviewRepository(context: RouteDataContext, place: Place) {
  const plans = await plansOf(context, place);
  const plan = plans.find((candidate) => candidate.start_from === place.environmentSlug) ?? plans[0];
  return plan === undefined ? null : repositoryOf(plan);
}

/**
 * The Environment a pull request's preview page opens from, for a link that names only the Project: the one its
 * repository's previews start from, else the Project's Default Environment.
 */
export async function requirePreviewEnvironment(context: RouteDataContext, place: Omit<Place, "environmentSlug">, repositoryId: string) {
  const plan = (await plansOf(context, place)).find((candidate) => String(candidate.repository_id) === repositoryId);
  if (plan === undefined) throw notFound();
  if (plan.start_from !== null) return plan.start_from;
  const project = (await requireStoreProjects(context, place.organizationSlug)).find((candidate) => candidate.name === place.projectSlug);
  if (!project?.default_environment) throw notFound();
  return project.default_environment;
}

/** A previews page's reads, started together: the repository's previews and the Project's Environments. */
export async function prefetchPreviews(context: RouteDataContext, organizationSlug: string, query: PreviewsQuery) {
  const scope = scopeOf(context);
  await prefetchRemote(context, previewsQueryOptions(organizationSlug, scope, query),
    storeViewOptions(organizationSlug, scope, environmentsQuery(query.project ?? "")));
}

/** The preview rule a start-from drafts: it asks the Servers which can hold a preview, so it never holds the page. */
export function streamPreviewRule(context: RouteDataContext, organizationSlug: string, query: PreviewRuleQuery) {
  streamRemote(context, previewRuleQueryOptions(organizationSlug, scopeOf(context), query));
}
