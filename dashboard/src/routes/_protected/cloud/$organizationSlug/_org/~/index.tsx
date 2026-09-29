import { useDeferredValue, useState } from "react";
import { useLiveQuery } from "@tanstack/react-db";
import { Link, createFileRoute, redirect } from "@tanstack/react-router";
import { PlusIcon } from "lucide-react";
import { ResourcePageControls } from "#/components/resource-page-controls";
import { DashboardPage } from "#/components/dashboard-page";
import { RouteErrorAlert } from "#/components/route-error-alert";
import { prefetchStoreViews, requireStoreProjects, requireWorkspace } from "#/collections/route-data";
import { storeEnabled } from "#/modules/config-store/store.contract";
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { getEnvironmentsCollection } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { buttonVariants } from "#/components/ui/button-variants";
import {
  Card,
  CardContent,
  CardHeader,
} from "#/components/ui/card";
import { Empty, EmptyDescription, EmptyHeader, EmptyTitle } from "#/components/ui/empty";
import { projectsQuery, requireView, servicesQuery, useStoreView } from "#/modules/config-store/store-view.queries";
import { Skeleton } from "#/components/ui/skeleton";
import { Route as EnvironmentOverviewRoute } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/index";
import { Route as NewProjectRoute } from "#/routes/_protected/cloud/$organizationSlug/_project/new";
import { ProjectCard } from "../-components/project-card";
import { useRuntimeServices } from "../../-components/services-online";

export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_org/~/")({
  loader: async ({ params, context }) => {
    if (storeEnabled) {
      const projects = await requireStoreProjects(context, params.organizationSlug);
      if (projects.length === 0) throw redirect({ to: NewProjectRoute.to, params: { organizationSlug: params.organizationSlug } });
      // Each card draws its Default Environment's Services.
      await prefetchStoreViews(context, params.organizationSlug, ...projects.map((project) =>
        servicesQuery({ project: project.name, environment: project.default_environment })));
      return;
    }
    const projects = await requireWorkspace(context, params.organizationSlug);
    if (projects.length === 0) {
      throw redirect({
        to: NewProjectRoute.to,
        params: { organizationSlug: params.organizationSlug },
      });
    }
  },
  pendingComponent: ProjectsPending,
  errorComponent: ProjectsError,
  component: RouteComponent,
});

function ProjectsPending() {
  return (
    <DashboardPage width="content">
      <Skeleton className="h-7 w-24" />
      <div className="flex items-center justify-between gap-3">
        <Skeleton className="h-9 w-full max-w-md" />
        <Skeleton className="h-9 w-28" />
      </div>
      <ProjectsGridPending />
    </DashboardPage>
  );
}

function ProjectsGridPending() {
  return (
      <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3" aria-label="Loading projects">
        {Array.from({ length: 3 }, (_, index) => (
          <Card key={index}>
            <CardHeader>
              <Skeleton className="h-5 w-36" />
            </CardHeader>
            <CardContent><Skeleton className="h-56 w-full" /></CardContent>
          </Card>
        ))}
      </div>
  );
}

function ProjectsError() {
  return (
    <DashboardPage width="content">
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
      className={buttonVariants({ variant: "ink" })}
    >
      <PlusIcon data-icon="inline-start" />
      New project
    </Link>
  );
}

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const [query, setQuery] = useState("");
  const deferredQuery = useDeferredValue(query);

  return (
    <DashboardPage width="content">
      {/* The top bar names the page. */}
      <div className="flex justify-end">
        <CreateProjectButton organizationSlug={organizationSlug} />
      </div>
      <div className="w-full sm:max-w-xs">
        <ResourcePageControls
          controlsLabel="Projects controls"
          searchAriaLabel="Search projects"
          searchPlaceholder="Search projects"
          searchValue={query}
          onSearchValueChange={setQuery}
        />
      </div>
      {storeEnabled
        ? <StoreProjectsGrid organizationSlug={organizationSlug} query={deferredQuery} />
        : <ProjectsGrid organizationSlug={organizationSlug} query={deferredQuery} />}
    </DashboardPage>
  );
}

/** The Config Store's Projects, each opening its Default Environment, drawn with that Environment's Services. */
function StoreProjectsGrid({ organizationSlug, query }: { organizationSlug: string; query: string }) {
  const projects = requireView(useStoreView(organizationSlug, projectsQuery())).projects;
  const normalizedQuery = query.trim().toLowerCase();
  const filtered = normalizedQuery ? projects.filter((project) => project.name.includes(normalizedQuery)) : projects;
  if (filtered.length === 0) return <NoMatchingProjects query={query} />;
  return (
    <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
      {filtered.map((project) => (
        <Link key={project.id} to={EnvironmentOverviewRoute.to}
          params={{ organizationSlug, projectSlug: project.name, environmentSlug: project.default_environment }}
          className="group/project block min-w-0 rounded-xl outline-none focus-visible:ring-3 focus-visible:ring-ring">
          <StoreProjectCard organizationSlug={organizationSlug} project={project.name} environment={project.default_environment} />
        </Link>
      ))}
    </div>
  );
}

function StoreProjectCard({ organizationSlug, project, environment }: { organizationSlug: string; project: string; environment: string }) {
  const listed = useStoreView(organizationSlug, servicesQuery({ project, environment }));
  const services = listed.ok ? listed.value.services : [];
  // ponytail: no online count yet; runtime evidence is matched by Namespace, which the Store names only once deployed.
  return <ProjectCard name={project} runtimeServices={[]} runtimeStatus="unavailable" environment={{
    name: environment, namespace: "",
    services: services.map((service) => ({ id: service.id, slug: service.name, config: { source: { type: service.source } } })),
  }} />;
}

// TODO(#1275): goes with the dark gate.
function ProjectsGrid({ organizationSlug, query }: { organizationSlug: string; query: string }) {
  const scope = useCollectionScope();
  const { data: environments } = useLiveQuery(getEnvironmentsCollection(organizationSlug, scope));
  const { runtimeServices, runtimeStatus } = useRuntimeServices(organizationSlug);
  const { projects } = useWorkspace(organizationSlug);
  const normalizedQuery = query.trim().toLowerCase();
  const filteredProjects = normalizedQuery
    ? projects.filter((project) => {
        const haystack = `${project.name} ${project.slug}`.toLowerCase();
        return haystack.includes(normalizedQuery);
      })
    : projects;

  return filteredProjects.length === 0 ? <NoMatchingProjects query={query} /> : (
        <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
          {filteredProjects.map((project) => {
            const resolvedEnvironment = project.resolvedEnvironment;
            const document = environments.find(environment => environment.id === resolvedEnvironment?.id);
            const card = (
              <ProjectCard
                name={project.name}
                environment={document ? { name: document.name, namespace: document.namespace, services: document.intent.services } : null}
                runtimeServices={runtimeServices}
                runtimeStatus={runtimeStatus}
              />
            );

            if (!resolvedEnvironment) {
              return (
                <div key={project.id}>
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
                className="group/project block min-w-0 rounded-xl outline-none focus-visible:ring-3 focus-visible:ring-ring"
              >
                {card}
              </Link>
            );
          })}
        </div>
      );
}

function NoMatchingProjects({ query }: { query: string }) {
  return (
    <Empty variant="no-results">
      <EmptyHeader>
        <EmptyTitle>No matching projects</EmptyTitle>
        <EmptyDescription>
          No projects match “{query.trim()}”.
        </EmptyDescription>
      </EmptyHeader>
    </Empty>
  );
}
