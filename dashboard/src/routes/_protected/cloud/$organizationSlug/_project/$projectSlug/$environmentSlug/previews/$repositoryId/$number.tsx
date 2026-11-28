import { createFileRoute, notFound } from "@tanstack/react-router";
import { prefetchPreviews, requirePreviewRepository } from "#/modules/previews/previews.route-data";
import { PreviewsError, PreviewsPending } from "../../-components/previews/preview-parts";
import { PreviewPage } from "../../-components/previews/PreviewPage";

/** One pull request's preview: what it is when live, else what one Deploy runs. The GitHub comment's link lands here. */
export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/previews/$repositoryId/$number")({
  staticData: { section: "previews" },
  loader: async ({ params, context }) => {
    if (!/^[1-9][0-9]{0,9}$/.test(params.number)) throw notFound();
    const repository = await requirePreviewRepository(context, params, params.repositoryId);
    await prefetchPreviews(context, params.organizationSlug, { project: params.projectSlug, repository: repository.repository, number: Number(params.number) });
    return { repository: repository.repository };
  },
  pendingComponent: PreviewsPending,
  errorComponent: PreviewsError,
  component: RouteComponent,
});

function RouteComponent() {
  const { number, ...params } = Route.useParams();
  return <PreviewPage params={params} repository={Route.useLoaderData().repository} number={Number(number)} />;
}
