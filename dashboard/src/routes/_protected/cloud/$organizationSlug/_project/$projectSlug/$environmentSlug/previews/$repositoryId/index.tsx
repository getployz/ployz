import { createFileRoute } from "@tanstack/react-router";
import { prefetchPreviews, requirePreviewRepository } from "#/modules/previews/previews.route-data";
import { PreviewsError, PreviewsPending } from "../../-components/previews/preview-parts";
import { PullRequestsPage } from "../../-components/previews/PullRequestsPage";

/** A repository's pull requests and their previews. */
export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/previews/$repositoryId/")({
  staticData: { section: "previews" },
  loader: async ({ params, context }) => {
    const repository = await requirePreviewRepository(context, params, params.repositoryId);
    await prefetchPreviews(context, params.organizationSlug, { project: params.projectSlug, repository: repository.repository, number: null });
    return { repository: repository.repository };
  },
  pendingComponent: PreviewsPending,
  errorComponent: PreviewsError,
  component: RouteComponent,
});

function RouteComponent() {
  return <PullRequestsPage params={Route.useParams()} repository={Route.useLoaderData().repository} />;
}
