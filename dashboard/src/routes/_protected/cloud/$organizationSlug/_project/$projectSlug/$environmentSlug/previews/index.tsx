import { createFileRoute, Link } from "@tanstack/react-router";
import { DashboardPage } from "#/components/dashboard-page";
import { Button } from "#/components/ui/button";
import { Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyTitle } from "#/components/ui/empty";
import { prefetchPreviews, requireDefaultPreviewRepository } from "#/modules/previews/previews.route-data";
import { PreviewsError, PreviewsPending } from "../-components/previews/preview-parts";
import { PullRequestsPage } from "../-components/previews/PullRequestsPage";

/** Pull requests from the rail: the repository that starts from this Environment, else the Project's first. */
export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/previews/")({
  staticData: { section: "previews" },
  loader: async ({ params, context }) => {
    const repository = await requireDefaultPreviewRepository(context, params);
    if (repository !== null) await prefetchPreviews(context, params.organizationSlug, { project: params.projectSlug, repository: repository.repository, number: null });
    return { repository };
  },
  pendingComponent: PreviewsPending,
  errorComponent: PreviewsError,
  component: RouteComponent,
});

function RouteComponent() {
  const params = Route.useParams();
  const { repository } = Route.useLoaderData();
  if (repository === null) {
    return (
      <DashboardPage width="content" title="Pull requests">
        <Empty variant="placeholder">
          <EmptyHeader>
            <EmptyTitle>No service deploys from GitHub</EmptyTitle>
            <EmptyDescription>Connect a service to a repository, and its pull requests show here.</EmptyDescription>
          </EmptyHeader>
          <EmptyContent>
            <Button variant="outline" render={<Link to="/cloud/$organizationSlug/$projectSlug/$environmentSlug" params={params} />}>Open the canvas</Button>
          </EmptyContent>
        </Empty>
      </DashboardPage>
    );
  }
  return <PullRequestsPage params={{ ...params, repositoryId: String(repository.repositoryId) }} repository={repository.repository} />;
}
