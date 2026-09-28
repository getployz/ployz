import { useState } from "react";
import { ENVIRONMENT_BRANCH_REVIEW_ROUTE_TO, ENVIRONMENT_NEW_BRANCH_ROUTE_TO } from "./-components/environment-route-paths";
import { Link, createFileRoute, useNavigate } from "@tanstack/react-router";
import { useLiveQuery } from "@tanstack/react-db";
import { ChevronRightIcon, GitBranchIcon, GitBranchPlusIcon, PlusIcon } from "lucide-react";
import { Effect, Option, Schema } from "effect";
import { getEnvironmentsCollection } from "#/collections/collections";
import { prefetchRemote, requireEnvironment } from "#/collections/route-data";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { DashboardPage } from "#/components/dashboard-page";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Field, FieldDescription, FieldLabel } from "#/components/ui/field";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { latestTeardownAttemptQueryOptions } from "#/modules/runtime/teardown.queries";
import { defaultEnvironmentRefusal } from "#/modules/runtime/teardown";
import { useSetDefaultEnvironment, useWorkspace } from "#/modules/environment-design/workspace.queries";
import { cn } from "#/lib/utils";
import { BranchIndent } from "#/components/environment-tree";
import { descendants, environmentTree } from "#/modules/project/environment-tree";
import { servicesOnline, useRuntimeServices } from "#/routes/_protected/cloud/$organizationSlug/-components/services-online";
import { TeardownDangerSection } from "#/routes/_protected/cloud/$organizationSlug/-components/teardown-danger-section";
import { useDeletionNodes, useEnvironmentPlace } from "#/routes/_protected/cloud/$organizationSlug/-components/deletion-items";
import { BranchDefaultsSection } from "./-components/branch-defaults-section";
import { CreateEnvironmentDialog } from "./-components/create-environment-dialog";
import { PrEnvironmentsSection } from "./-components/pr-environments-section";
import { missingPrEnvironmentGrantsQueryOptions } from "#/modules/pr-environments/plan.queries";
import { prEnvironmentIds } from "#/modules/pr-environments/pull-request";
import { useEnvironmentNotes } from "#/modules/project/environment-notes";
import { Route as EnvironmentLayoutRoute } from "./route";

const settingsSection = Schema.Literals(["environment", "project"]);

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/settings",
)({
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({
    scope: Schema.optional(settingsSection.pipe(Schema.catchDecoding(() => Effect.succeed(Option.none())))),
  })),
  loader: async ({ params, context }) => {
    const { organizationSlug, projectSlug } = params;
    const environment = await requireEnvironment(context, params);
    await prefetchRemote(context,
      latestTeardownAttemptQueryOptions({ organizationSlug, scope: "environment", environmentId: environment.id }),
      latestTeardownAttemptQueryOptions({ organizationSlug, scope: "project", projectSlug }),
      missingPrEnvironmentGrantsQueryOptions(organizationSlug));
  },
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug, projectSlug, environmentSlug } = Route.useParams();
  const { scope: section = "environment" } = Route.useSearch();
  const { environmentId } = EnvironmentLayoutRoute.useLoaderData();
  const navigate = useNavigate({ from: Route.fullPath });
  const { projects, environments, branches } = useWorkspace(organizationSlug);
  const project = projects.find((row) => row.slug === projectSlug);
  const environment = environments.find((row) => row.id === environmentId);
  const branch = branches.find((row) => row.environmentId === environmentId);
  // A teardown takes the Environment's Branches with it, deepest first.
  const closing = descendants(environmentId, branches).flatMap((id) => environments.filter((row) => row.id === id));
  const defaultEnvironment = [...closing, environment].find((row) => row !== undefined && row.id === project?.defaultEnvironmentId);
  const own = useDeletionNodes(organizationSlug, { projectSlug, environmentSlug });
  const projectNodes = useDeletionNodes(organizationSlug, { projectSlug });
  const place = useEnvironmentPlace(organizationSlug, environmentId);
  const name = environment?.name ?? environmentSlug;
  const projectName = project?.name ?? projectSlug;

  // The project opens its Default Environment, which can't be deleted.
  function leaveDeletedEnvironment() {
    void navigate({ to: "/cloud/$organizationSlug/$projectSlug", params: { organizationSlug, projectSlug }, replace: true });
  }

  function leaveDeletedProject() {
    void navigate({ to: "/cloud/$organizationSlug/~", params: { organizationSlug }, replace: true });
  }

  return (
    <DashboardPage width="content">
      {/* The rail, or on phones the strip under the top bar, picks the section. */}
      {section === "environment" ? (
        <div className="flex flex-col gap-8">
          {branch && <Item variant="outline" size="sm" render={<Link to={ENVIRONMENT_BRANCH_REVIEW_ROUTE_TO}
            params={{ organizationSlug, projectSlug, environmentSlug }} />}>
            <ItemMedia variant="icon"><GitBranchIcon /></ItemMedia>
            <ItemContent><ItemTitle>Manage {name}</ItemTitle>
              <ItemDescription>{branch.pullRequest ? "Save, shut down or close it" : "Save, keep or close it"} from its panel.</ItemDescription></ItemContent>
            <ItemActions><ChevronRightIcon className="size-4 text-muted-foreground" /></ItemActions>
          </Item>}
          <BranchDefaultsSection organizationSlug={organizationSlug} environmentId={environmentId} />
          {!branch && <TeardownDangerSection
            organizationSlug={organizationSlug}
            scope="environment"
            environmentId={environmentId}
            name={name}
            place={place}
            title="Delete this environment"
            description="Its services, data and branches go with it."
            actionLabel="Delete environment"
            items={[...own, ...closing.map((row) => ({ kind: "branch" as const, name: row.name }))]}
            disabledReason={defaultEnvironment && defaultEnvironmentRefusal(defaultEnvironment.name)}
            headingId="environment-teardown-heading"
            onCompleted={leaveDeletedEnvironment}
          />}
        </div>
      ) : (
        <div className="flex flex-col gap-8">
          {project && <ProjectSettings organizationSlug={organizationSlug} project={project} branches={branches} environmentSlug={environmentSlug} />}
          <TeardownDangerSection
            organizationSlug={organizationSlug}
            scope="project"
            projectSlug={projectSlug}
            name={projectName}
            place={projectName}
            title="Delete this project"
            description="Its environments, services and data go with it."
            actionLabel="Delete project"
            items={[
              ...environments.filter((row) => row.projectId === project?.id).map((row) => ({
                kind: branches.some((candidate) => candidate.environmentId === row.id) ? "branch" as const : "environment" as const,
                name: row.name,
              })),
              ...projectNodes,
            ]}
            headingId="project-teardown-heading"
            onCompleted={leaveDeletedProject}
          />
        </div>
      )}
    </DashboardPage>
  );
}

function ProjectSettings({ organizationSlug, project, branches, environmentSlug }: {
  organizationSlug: string;
  /** The current Environment, which "New branch" branches. */
  environmentSlug: string;
  project: ReturnType<typeof useWorkspace>["projects"][number];
  branches: ReturnType<typeof useWorkspace>["branches"];
}) {
  const [creating, setCreating] = useState(false);
  const scope = useCollectionScope();
  const { data: allEnvironments } = useLiveQuery(getEnvironmentsCollection(organizationSlug, scope));
  const { runtimeServices, runtimeStatus } = useRuntimeServices(organizationSlug);
  const setDefaultEnvironment = useSetDefaultEnvironment(organizationSlug);
  const environments = allEnvironments
    .filter((environment) => environment.projectId === project.id)
    .sort((a, b) => a.createdAt.getTime() - b.createdAt.getTime());
  const defaultEnvironment = project.resolvedEnvironment;
  // A PR Environment can't be the Default Environment.
  const prEnvironments = prEnvironmentIds(branches);
  const notesOf = useEnvironmentNotes(organizationSlug, defaultEnvironment?.id);

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
                {environments.filter((environment) => !prEnvironments.has(environment.id)).map((environment) => (
                  <SelectItem key={environment.id} value={environment.id} label={environment.name}>{environment.name}</SelectItem>
                ))}
              </SelectGroup>
            </SelectContent>
          </Select>
          <FieldDescription>Where {project.name} opens, for everyone in this organization.</FieldDescription>
        </Field>
      </section>
      <section aria-labelledby="project-environments-heading" className="flex flex-col gap-4">
        <div className="flex flex-wrap items-center justify-between gap-4">
          <h2 id="project-environments-heading" className="text-base font-semibold">Environments</h2>
          <div className="flex gap-2">
            <Button variant="outline" nativeButton={false} render={<Link to={ENVIRONMENT_NEW_BRANCH_ROUTE_TO}
              params={{ organizationSlug, projectSlug: project.slug, environmentSlug }} />}>
              <GitBranchPlusIcon data-icon="inline-start" />New branch
            </Button>
            <Button variant="outline" onClick={() => setCreating(true)}><PlusIcon data-icon="inline-start" />New environment</Button>
          </div>
        </div>
        <ItemGroup className="gap-2">
          {environmentTree(environments, branches).map(({ environment, depth, parent }) => {
            const { online, label } = servicesOnline({ namespace: environment.namespace, services: environment.intent.services }, runtimeServices, runtimeStatus);
            // The Default chip says "default" already.
            const notes = notesOf(environment.id).filter((note) => note !== "default");
            return (
              <Item key={environment.id} variant="outline" size="sm" render={
                <Link to="/cloud/$organizationSlug/$projectSlug/$environmentSlug"
                  params={{ organizationSlug, projectSlug: project.slug, environmentSlug: environment.namespace }} />
              }>
                <ItemContent className="min-w-0">
                  <ItemTitle className="min-w-0">
                    <BranchIndent depth={depth} />
                    <span className="truncate">{environment.name}</span>
                    {parent && <span className="sr-only">, branch of {parent.name}</span>}
                    {environment.id === defaultEnvironment?.id && <Badge variant="secondary">Default</Badge>}
                  </ItemTitle>
                  <ItemDescription className="flex items-center gap-2">
                    <span aria-hidden="true" className={cn("size-2 shrink-0 rounded-full", online ? "bg-success" : "bg-muted-foreground")} />
                    <span className="truncate">{[label, ...notes].join(" · ")}</span>
                  </ItemDescription>
                </ItemContent>
                <ItemActions><ChevronRightIcon className="size-4 text-muted-foreground" /></ItemActions>
              </Item>
            );
          })}
        </ItemGroup>
      </section>
      <PrEnvironmentsSection organizationSlug={organizationSlug} project={project} environments={environments} branches={branches} />
      {creating && <CreateEnvironmentDialog onOpenChange={setCreating} organizationSlug={organizationSlug} projectSlug={project.slug} />}
    </>
  );
}
