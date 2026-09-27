import { Link, createFileRoute, useNavigate } from "@tanstack/react-router";
import { useLiveQuery } from "@tanstack/react-db";
import { ChevronRightIcon } from "lucide-react";
import { Effect, Option, Schema } from "effect";
import { getEnvironmentsCollection } from "#/collections/collections";
import { prefetchRemote, requireEnvironment } from "#/collections/route-data";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { DashboardPage } from "#/components/dashboard-page";
import { Badge } from "#/components/ui/badge";
import { Field, FieldDescription, FieldLabel } from "#/components/ui/field";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "#/components/ui/tabs";
import { latestTeardownAttemptQueryOptions } from "#/modules/runtime/teardown.queries";
import { useSetDefaultEnvironment, useWorkspace } from "#/modules/environment-design/workspace.queries";
import { cn } from "#/lib/utils";
import { servicesOnline, useRuntimeServices } from "#/routes/_protected/cloud/$organizationSlug/-components/services-online";
import { TeardownDangerSection } from "#/routes/_protected/cloud/$organizationSlug/-components/teardown-danger-section";
import { Route as EnvironmentLayoutRoute } from "./route";

const settingsTab = Schema.Literals(["environment", "project"]);

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/settings",
)({
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({
    scope: Schema.optional(settingsTab.pipe(Schema.catchDecoding(() => Effect.succeed(Option.none())))),
  })),
  loader: async ({ params, context }) => {
    const { organizationSlug, projectSlug } = params;
    const environment = await requireEnvironment(context, params);
    await prefetchRemote(context,
      latestTeardownAttemptQueryOptions({ organizationSlug, scope: "environment", environmentId: environment.id }),
      latestTeardownAttemptQueryOptions({ organizationSlug, scope: "project", projectSlug }));
  },
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug, projectSlug, environmentSlug } = Route.useParams();
  const { scope: tab = "environment" } = Route.useSearch();
  const { environmentId } = EnvironmentLayoutRoute.useLoaderData();
  const navigate = useNavigate({ from: Route.fullPath });
  const { projects, environments } = useWorkspace(organizationSlug);
  const project = projects.find((row) => row.slug === projectSlug);
  const environment = environments.find((row) => row.id === environmentId);

  function leaveDeletedTree() {
    void navigate({
      to: "/cloud/$organizationSlug/~",
      params: { organizationSlug },
      replace: true,
    });
  }

  return (
    <DashboardPage width="content">
      <h1 className="text-xl font-semibold">Settings</h1>
      <Tabs value={tab} onValueChange={(value) => {
        if (Schema.is(settingsTab)(value)) void navigate({ search: { scope: value }, replace: true });
      }}>
        <TabsList variant="line">
          <TabsTrigger value="environment">
            Environment <span className="text-muted-foreground">{environment?.name ?? environmentSlug}</span>
          </TabsTrigger>
          <TabsTrigger value="project">
            Project <span className="text-muted-foreground">{project?.name ?? projectSlug}</span>
          </TabsTrigger>
        </TabsList>
        <TabsContent value="environment" className="mt-6">
          <TeardownDangerSection
            organizationSlug={organizationSlug}
            scope="environment"
            environmentId={environmentId}
            confirmPhrase={environment?.name ?? environmentSlug}
            title="Tear down this environment"
            description="Deletes this environment and all of its services and volumes. This cannot be undone."
            actionLabel="Tear down environment"
            headingId="environment-teardown-heading"
            onCompleted={leaveDeletedTree}
          />
        </TabsContent>
        <TabsContent value="project" className="mt-6 flex flex-col gap-8">
          {project && <ProjectSettings organizationSlug={organizationSlug} project={project} />}
          <TeardownDangerSection
            organizationSlug={organizationSlug}
            scope="project"
            projectSlug={projectSlug}
            confirmPhrase={project?.name ?? projectSlug}
            title="Tear down this project"
            description="Deletes this project and all of its environments, services, and volumes. This cannot be undone."
            actionLabel="Tear down project"
            headingId="project-teardown-heading"
            onCompleted={leaveDeletedTree}
          />
        </TabsContent>
      </Tabs>
    </DashboardPage>
  );
}

function ProjectSettings({ organizationSlug, project }: {
  organizationSlug: string;
  project: ReturnType<typeof useWorkspace>["projects"][number];
}) {
  const scope = useCollectionScope();
  const { data: allEnvironments } = useLiveQuery(getEnvironmentsCollection(organizationSlug, scope));
  const { runtimeServices, runtimeStatus } = useRuntimeServices(organizationSlug);
  const setDefaultEnvironment = useSetDefaultEnvironment(organizationSlug);
  const environments = allEnvironments
    .filter((environment) => environment.projectId === project.id)
    .sort((a, b) => a.createdAt.getTime() - b.createdAt.getTime());
  const defaultEnvironment = project.resolvedEnvironment;

  return (
    <>
      <section aria-labelledby="project-heading" className="flex flex-col gap-4">
        <h2 id="project-heading" className="text-lg font-semibold">{project.name}</h2>
        <Field>
          <FieldLabel htmlFor="default-environment">Default environment</FieldLabel>
          <Select value={defaultEnvironment?.id ?? null} onValueChange={(next) => {
            if (next && next !== defaultEnvironment?.id) setDefaultEnvironment(project, next);
          }}>
            <SelectTrigger id="default-environment" className="w-full sm:max-w-sm">
              <SelectValue>{defaultEnvironment?.name}</SelectValue>
            </SelectTrigger>
            <SelectContent>
              <SelectGroup>
                {environments.map((environment) => (
                  <SelectItem key={environment.id} value={environment.id} label={environment.name}>{environment.name}</SelectItem>
                ))}
              </SelectGroup>
            </SelectContent>
          </Select>
          <FieldDescription>Where {project.name} opens, for everyone in this organization.</FieldDescription>
        </Field>
      </section>
      <section aria-labelledby="project-environments-heading" className="flex flex-col gap-4">
        <h2 id="project-environments-heading" className="text-base font-semibold">Environments</h2>
        <ItemGroup className="gap-2">
          {environments.map((environment) => {
            const { online, label } = servicesOnline({ namespace: environment.namespace, services: environment.intent.services }, runtimeServices, runtimeStatus);
            return (
              <Item key={environment.id} variant="outline" size="sm" render={
                <Link to="/cloud/$organizationSlug/$projectSlug/$environmentSlug"
                  params={{ organizationSlug, projectSlug: project.slug, environmentSlug: environment.namespace }} />
              }>
                <ItemContent className="min-w-0">
                  <ItemTitle className="min-w-0">
                    <span className="truncate">{environment.name}</span>
                    {environment.id === defaultEnvironment?.id && <Badge variant="secondary">Default</Badge>}
                  </ItemTitle>
                  <ItemDescription className="flex items-center gap-2">
                    <span aria-hidden="true" className={cn("size-2 shrink-0 rounded-full", online ? "bg-success" : "bg-muted-foreground")} />
                    {label}
                  </ItemDescription>
                </ItemContent>
                <ItemActions><ChevronRightIcon className="size-4 text-muted-foreground" /></ItemActions>
              </Item>
            );
          })}
        </ItemGroup>
      </section>
    </>
  );
}
