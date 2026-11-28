import { createFileRoute } from "@tanstack/react-router";
import { Schema } from "effect";
import { prefetchPreviews, requirePreviewRepository } from "#/modules/previews/previews.route-data";
import { PreviewsError, PreviewsPending } from "../../-components/previews/preview-parts";
import { ClosePreviewPage } from "../../-components/previews/ClosePreviewPage";

/** The live previews, to close one; `for` names the pull request that waits for room. */
export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/previews/$repositoryId/close")({
  staticData: { section: "previews" },
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({ for: Schema.optional(Schema.Number) })),
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
  return <ClosePreviewPage params={Route.useParams()} repository={Route.useLoaderData().repository} wants={Route.useSearch().for ?? null} />;
}
