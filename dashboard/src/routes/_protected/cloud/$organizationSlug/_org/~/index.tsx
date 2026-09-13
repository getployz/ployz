import { useDeferredValue, useState } from "react";
import { useSuspenseQuery } from "@tanstack/react-query";
import { Link, createFileRoute } from "@tanstack/react-router";
import {
  FolderOpenIcon,
  PlusIcon,
} from "lucide-react";
import { ResourcePageControls } from "#/components/resource-page-controls";
import { DashboardPage } from "#/components/dashboard-page";
import { RouteErrorAlert } from "#/components/route-error-alert";
import { projectListQueryOptions } from "#/modules/environment-design/workspace-queries";
import { buttonVariants } from "#/components/ui/button-variants";
import {
  Card,
  CardContent,
  CardFooter,
  CardHeader,
  CardTitle,
} from "#/components/ui/card";
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "#/components/ui/empty";
import { Skeleton } from "#/components/ui/skeleton";
import { cn } from "#/lib/utils";
import { Route as EnvironmentOverviewRoute } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/index";
import { Route as NewProjectRoute } from "#/routes/_protected/cloud/$organizationSlug/_project/new";

export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_org/~/")({
  loader: ({ params, context }) =>
    context.queryClient.ensureQueryData(
      projectListQueryOptions(params.organizationSlug),
    ),
  pendingComponent: ProjectsPending,
  errorComponent: ProjectsError,
  component: RouteComponent,
});

function ProjectsPending() {
  return (
    <DashboardPage>
      <div className="flex items-center justify-between gap-3">
        <Skeleton className="h-9 w-full max-w-md" />
        <Skeleton className="h-9 w-28" />
      </div>
      <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
        {Array.from({ length: 3 }, (_, index) => (
          <Card key={index} className="min-h-52">
            <CardHeader>
              <Skeleton className="h-5 w-36" />
            </CardHeader>
            <CardContent className="flex-1">
              <Skeleton className="h-4 w-24" />
            </CardContent>
            <CardFooter className="gap-2">
              <Skeleton className="size-2 rounded-full" />
              <Skeleton className="h-3 w-24" />
            </CardFooter>
          </Card>
        ))}
      </div>
    </DashboardPage>
  );
}

function ProjectsError() {
  return (
    <DashboardPage>
      <RouteErrorAlert
        title="Projects couldn’t load"
        description="The project list is unavailable right now. Try loading it again."
      />
    </DashboardPage>
  );
}

function CreateProjectButton({
  organizationSlug,
}: {
  organizationSlug: string;
}) {
  return (
    <Link
      to={NewProjectRoute.to}
      params={{ organizationSlug }}
      className={buttonVariants({ size: "lg" })}
    >
      <PlusIcon data-icon="inline-start" />
      Project
    </Link>
  );
}

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const [query, setQuery] = useState("");
  const deferredQuery = useDeferredValue(query);
  const { data: projects } = useSuspenseQuery(
    projectListQueryOptions(organizationSlug),
  );
  const normalizedQuery = deferredQuery.trim().toLowerCase();
  const filteredProjects = normalizedQuery
    ? projects.filter((project) => {
        const haystack = `${project.name} ${project.slug}`.toLowerCase();
        return haystack.includes(normalizedQuery);
      })
    : projects;

  return (
    <DashboardPage>
      <h1 className="sr-only">Projects</h1>

      <ResourcePageControls
        controlsLabel="Projects controls"
        searchAriaLabel="Search projects"
        searchPlaceholder="Search projects"
        searchValue={query}
        onSearchValueChange={setQuery}
        action={<CreateProjectButton organizationSlug={organizationSlug} />}
      />

      {projects.length === 0 ? (
        <Empty variant="first-run">
          <EmptyHeader>
            <EmptyMedia variant="icon">
              <FolderOpenIcon />
            </EmptyMedia>
            <EmptyTitle>No projects yet</EmptyTitle>
            <EmptyDescription>
              Create a project to start adding environments and services.
            </EmptyDescription>
          </EmptyHeader>
          <EmptyContent className="sm:flex-row sm:justify-center">
            <CreateProjectButton organizationSlug={organizationSlug} />
          </EmptyContent>
        </Empty>
      ) : filteredProjects.length === 0 ? (
        <Empty variant="no-results">
          <EmptyHeader>
            <EmptyTitle>No matching projects</EmptyTitle>
            <EmptyDescription>
              No projects match “{deferredQuery.trim()}”.
            </EmptyDescription>
          </EmptyHeader>
        </Empty>
      ) : (
        <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
          {filteredProjects.map((project) => {
            const resolvedEnvironment = project.resolvedEnvironment;
            const card = (
              <Card className="flex min-h-52 flex-col">
                <CardHeader>
                  <CardTitle>{project.name}</CardTitle>
                </CardHeader>
                <CardContent className="flex-1" />
                <CardFooter className="gap-2">
                  {resolvedEnvironment ? (
                    <>
                      <span className="size-2 shrink-0 rounded-full bg-primary" />
                      <span className="text-xs text-muted-foreground">
                        {resolvedEnvironment.name.toLowerCase()}
                      </span>
                    </>
                  ) : (
                    <>
                      <span className="size-2 shrink-0 rounded-full bg-muted-foreground" />
                      <span className="text-xs text-muted-foreground">
                        No services
                      </span>
                    </>
                  )}
                </CardFooter>
              </Card>
            );

            if (!resolvedEnvironment) {
              return (
                <div key={project.id} className={cn("pointer-events-none")}>
                  {card}
                </div>
              );
            }

            return (
              <Link
                key={project.id}
                to={EnvironmentOverviewRoute.to}
                params={{
                  organizationSlug,
                  projectSlug: project.slug,
                  environmentSlug: resolvedEnvironment.namespace,
                }}
                className="block"
              >
                {card}
              </Link>
            );
          })}
        </div>
      )}
    </DashboardPage>
  );
}
