import { createFileRoute } from "@tanstack/react-router";
import { Schema } from "effect";
import { prefetchPreviews, requirePreviewRepository, streamPreviewRule } from "#/modules/previews/previews.route-data";
import { PreviewsError, PreviewsPending } from "../../-components/previews/preview-parts";
import { PreviewSettingsPage } from "../../-components/previews/PreviewSettingsPage";

/** A repository's Preview settings; `edit` opens them for editing, as Set up previews and Raise the limit do. */
export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/previews/$repositoryId/settings")({
  staticData: { section: "previews" },
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({ edit: Schema.optional(Schema.Boolean) })),
  loader: async ({ params, context }) => {
    const repository = await requirePreviewRepository(context, params, params.repositoryId);
    const named = repository.repository;
    // The rule a start-from drafts asks the Servers, so it streams; the page reads it again for another start-from.
    streamPreviewRule(context, params.organizationSlug, { project: params.projectSlug, repository: named, start_from: repository.startFrom ?? params.environmentSlug });
    await prefetchPreviews(context, params.organizationSlug, { project: params.projectSlug, repository: named, number: null });
    return { repository: named, repositoryId: repository.repositoryId };
  },
  pendingComponent: PreviewsPending,
  errorComponent: PreviewsError,
  component: RouteComponent,
});

function RouteComponent() {
  const { repository, repositoryId } = Route.useLoaderData();
  return <PreviewSettingsPage params={Route.useParams()} repository={repository} repositoryId={repositoryId} edit={Route.useSearch().edit === true} />;
}
