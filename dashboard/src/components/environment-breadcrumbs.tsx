import { Fragment, useState, type ReactNode } from "react";
import { useLiveQuery } from "@tanstack/react-db";
import { Link, useNavigate } from "@tanstack/react-router";
import { ChevronsUpDownIcon, GitBranchIcon, GitBranchPlusIcon, GitCompareArrowsIcon, LayoutGridIcon, MoreHorizontalIcon, Settings2Icon } from "lucide-react";
import { getEnvironmentDeploymentsCollection } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import {
  getDashboardDestination,
  getDashboardSectionLabel,
  type DashboardDestination,
  type DashboardScope,
} from "#/components/dashboard-navigation-model";
import { BranchIndent } from "#/components/environment-tree";
import { useDashboardSection } from "#/components/use-dashboard-section";
import { Breadcrumb, BreadcrumbItem, BreadcrumbList, BreadcrumbPage, BreadcrumbSeparator } from "#/components/ui/breadcrumb";
import { Button } from "#/components/ui/button";
import { Popover, PopoverContent, PopoverTitle, PopoverTrigger } from "#/components/ui/popover";
import { Command, CommandGroup, CommandItem, CommandList, CommandSeparator } from "#/components/ui/command";
import { Skeleton } from "#/components/ui/skeleton";
import { findEnvironment, useWorkspace } from "#/modules/environment-design/workspace.queries";
import { environmentTree } from "#/modules/project/environment-tree";
import { useBranchReviews } from "#/modules/branches/use-branch-review";

type EnvironmentScope = Extract<DashboardScope, { kind: "environment" }>;

/**
 * `project / environment`, each crumb a switcher; places other than Canvas add their name. A Branch reads
 * `project / parent ⑂ branch`, and its Parent's crumb opens the Parent.
 */
export function EnvironmentCrumbs({ scope }: { scope: EnvironmentScope }) {
  const section = useDashboardSection();
  const { projects, environments, branches } = useWorkspace(scope.organizationSlug);
  const current = findEnvironment(projects, environments, scope);
  const parentId = branches.find((branch) => branch.environmentId === current?.id)?.parentEnvironmentId;
  const parent = environments.find((environment) => environment.id === parentId);
  return <Crumbs forkAt={parent ? 2 : undefined} items={[
    <ProjectCrumb key="project" scope={scope} />,
    ...parent ? [
      <Button key="parent" variant="ghost" size="sm" className="min-w-0" title={parent.name} aria-label={`Parent: ${parent.name}`}
        render={<Link {...getDashboardDestination({ ...scope, environmentSlug: parent.namespace }, section)} />}>
        <span className="truncate">{parent.name}</span>
      </Button>,
    ] : [],
    <EnvironmentCrumb key="environment" scope={scope} />,
    ...section === "canvas" ? [] : [<BreadcrumbPage key="place" className="px-2 font-medium">{getDashboardSectionLabel(section)}</BreadcrumbPage>],
  ]} />;
}

/**
 * On phones the path keeps its last two crumbs; the rest move into a "…" menu so the bar never wraps.
 * `forkAt` marks the crumb a Branch starts at: ⑂ separates it from its Parent instead of "/".
 */
export function Crumbs({ items, forkAt }: { items: ReactNode[]; forkAt?: number }) {
  const collapsed = items.slice(0, -2);
  return (
    <Breadcrumb aria-label="Breadcrumb" className="min-w-0">
      <BreadcrumbList className="flex-nowrap">
        {collapsed.length ? <>
          <BreadcrumbItem className="min-wf-nav:hidden">
            <Popover>
              <PopoverTrigger render={<Button variant="ghost" size="icon-sm" aria-label="More breadcrumbs" title="More breadcrumbs" />}>
                <MoreHorizontalIcon />
              </PopoverTrigger>
              <PopoverContent align="start" className="w-auto">
                <PopoverTitle className="sr-only">More breadcrumbs</PopoverTitle>
                <div className="flex flex-col items-start gap-1">{collapsed}</div>
              </PopoverContent>
            </Popover>
          </BreadcrumbItem>
          <BreadcrumbSeparator className="min-wf-nav:hidden">/</BreadcrumbSeparator>
        </> : null}
        {items.map((item, index) => <Fragment key={index}>
          {/* The "…" menu brings its own separator, so the first visible crumb's is desktop-only too. */}
          {index > 0 ? <BreadcrumbSeparator className={index <= collapsed.length ? phonesHidden : undefined}>
            {index === forkAt ? <GitBranchIcon /> : "/"}
          </BreadcrumbSeparator> : null}
          <BreadcrumbItem className={index < collapsed.length ? phonesHidden : "min-w-0"}>{item}</BreadcrumbItem>
        </Fragment>)}
      </BreadcrumbList>
    </Breadcrumb>
  );
}

const phonesHidden = "hidden min-wf-nav:inline-flex";

function CrumbTrigger({ label, name, current }: { label: string; name: string; current?: boolean }) {
  return (
    <PopoverTrigger render={<Button variant={current ? "outline" : "ghost"} size="sm"
      aria-label={`${label}: ${name}`} title={name} className="min-w-0" />}>
      <span className="truncate">{name}</span>
      <ChevronsUpDownIcon data-icon="inline-end" />
    </PopoverTrigger>
  );
}

function SwitcherLoading({ isPending, retry, children }: { isPending: boolean; retry?: () => void; children: ReactNode }) {
  if (isPending) return (
    <div role="status" aria-label="Loading" className="flex flex-col gap-2 p-2">
      <Skeleton className="h-7 w-full" /><Skeleton className="h-7 w-full" />
    </div>
  );
  if (retry) return <Button variant="ghost" onClick={retry}>Could not load projects. Retry</Button>;
  return children;
}

function ProjectCrumb({ scope }: { scope: EnvironmentScope }) {
  const { projects, isPending, isError, refetch } = useWorkspace(scope.organizationSlug);
  const navigate = useNavigate();
  const section = useDashboardSection();
  const [open, setOpen] = useState(false);
  const project = projects.find((candidate) => candidate.slug === scope.projectSlug);
  const go = (destination: DashboardDestination) => { setOpen(false); void navigate(destination); };
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <CrumbTrigger label="Project" name={project?.name ?? scope.projectSlug} />
      <PopoverContent padding="none" align="start" className="w-[min(20rem,calc(100vw-2rem))]">
        <PopoverTitle className="sr-only">Switch project</PopoverTitle>
        <SwitcherLoading isPending={isPending} retry={isError ? () => void refetch() : undefined}>
          <Command tabIndex={0} label="Projects" defaultValue={project?.id}>
            <CommandList className="max-h-[min(20rem,45dvh)]">
              <CommandGroup heading="Projects">
                {projects.map((candidate) => {
                  // #1141 owns which Environment a project opens; the switcher only shows it.
                  const environment = candidate.resolvedEnvironment;
                  return <CommandItem key={candidate.id} value={candidate.id} keywords={[candidate.name]}
                    disabled={!environment} data-checked={candidate.slug === scope.projectSlug}
                    aria-label={environment ? `${candidate.name}, opens ${environment.name}` : `${candidate.name}, no environments`}
                    onSelect={() => {
                      if (!environment) return;
                      go(getDashboardDestination({ ...scope, projectSlug: candidate.slug, environmentSlug: environment.namespace }, section));
                    }}>
                    <span className="flex-1 truncate">{candidate.name}</span>
                    <span className="truncate text-muted-foreground">{environment?.name ?? "No environments"}</span>
                  </CommandItem>;
                })}
              </CommandGroup>
              <CommandSeparator />
              <CommandGroup>
                <CommandItem value="all-projects" onSelect={() => go(getDashboardDestination({ kind: "all", organizationSlug: scope.organizationSlug }, "projects"))}>
                  <LayoutGridIcon />All projects
                </CommandItem>
              </CommandGroup>
            </CommandList>
          </Command>
        </SwitcherLoading>
      </PopoverContent>
    </Popover>
  );
}

function EnvironmentCrumb({ scope }: { scope: EnvironmentScope }) {
  const { projects, environments, branches, isPending, isError, refetch } = useWorkspace(scope.organizationSlug);
  const { data: deployments } = useLiveQuery(getEnvironmentDeploymentsCollection(scope.organizationSlug, useCollectionScope()));
  const reviewOf = useBranchReviews(scope.organizationSlug);
  const navigate = useNavigate();
  const section = useDashboardSection();
  const [open, setOpen] = useState(false);
  const project = projects.find((candidate) => candidate.slug === scope.projectSlug);
  const current = findEnvironment(projects, environments, scope);
  const tree = environmentTree(environments.filter((environment) => environment.projectId === project?.id), branches);
  // The Org Store keeps each Environment's latest attempt, so having none means it was never deployed.
  const deployed = new Set(deployments.map((deployment) => deployment.environmentId));
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <CrumbTrigger label="Environment" name={current?.name ?? scope.environmentSlug} current />
      <PopoverContent padding="none" align="start" className="w-[min(20rem,calc(100vw-2rem))]">
        <PopoverTitle className="sr-only">Switch environment</PopoverTitle>
        <SwitcherLoading isPending={isPending} retry={isError ? () => void refetch() : undefined}>
          <Command tabIndex={0} label="Environments" defaultValue={current?.id}>
            <CommandList className="max-h-[min(20rem,45dvh)]">
              <CommandGroup heading="Environments">
                {tree.map(({ environment, depth, parent }) => {
                  // Computed only while the switcher is open: core compares each Branch with its Parent.
                  const review = parent ? reviewOf(environment.id) : null;
                  const notes = [
                    environment.id === project?.resolvedEnvironment?.id && "default",
                    !deployed.has(environment.id) && "not deployed",
                    !!review?.changes && `${review.changes} ${review.changes === 1 ? "change" : "changes"}`,
                    !!review?.updates && `${review.updates} ${review.updates === 1 ? "update" : "updates"}`,
                  ].filter((note) => note !== false);
                  return (
                    <CommandItem key={environment.id} value={environment.id} keywords={[environment.name]}
                      data-checked={environment.id === current?.id}
                      aria-label={[environment.name, parent && `branch of ${parent.name}`, ...notes].filter(Boolean).join(", ")}
                      onSelect={() => {
                        setOpen(false);
                        void navigate(getDashboardDestination({ ...scope, environmentSlug: environment.namespace }, section));
                      }}>
                      <BranchIndent depth={depth} />
                      <span className="flex-1 truncate">{environment.name}</span>
                      {notes.length > 0 && <span className="shrink-0 text-muted-foreground">{notes.join(" · ")}</span>}
                    </CommandItem>
                  );
                })}
              </CommandGroup>
              <CommandSeparator />
              <CommandGroup>
                {current && branches.some((branch) => branch.environmentId === current.id) && <CommandItem value="review" onSelect={() => {
                  setOpen(false);
                  const { organizationSlug, projectSlug, environmentSlug } = scope;
                  void navigate({ to: "/cloud/$organizationSlug/$projectSlug/$environmentSlug/review",
                    params: { organizationSlug, projectSlug, environmentSlug } });
                }}>
                  <GitCompareArrowsIcon />Review {current.name}
                </CommandItem>}
                {current && <CommandItem value="new-branch" onSelect={() => {
                  setOpen(false);
                  const { organizationSlug, projectSlug, environmentSlug } = scope;
                  void navigate({ to: "/cloud/$organizationSlug/$projectSlug/$environmentSlug/new-branch",
                    params: { organizationSlug, projectSlug, environmentSlug } });
                }}>
                  <GitBranchPlusIcon />New branch of {current.name}
                </CommandItem>}
                <CommandItem value="manage-environments" onSelect={() => {
                  setOpen(false);
                  const { organizationSlug, projectSlug, environmentSlug } = scope;
                  void navigate({ to: "/cloud/$organizationSlug/$projectSlug/$environmentSlug/settings",
                    params: { organizationSlug, projectSlug, environmentSlug }, search: { scope: "project" } });
                }}>
                  <Settings2Icon />Manage environments
                </CommandItem>
              </CommandGroup>
            </CommandList>
          </Command>
        </SwitcherLoading>
      </PopoverContent>
    </Popover>
  );
}
