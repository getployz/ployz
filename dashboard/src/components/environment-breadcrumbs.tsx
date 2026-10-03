import { Fragment, useState, type ReactNode } from "react";
import { ENVIRONMENT_NEW_BRANCH_ROUTE_TO } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/environment-route-paths";
import { Link, useLocation, useNavigate } from "@tanstack/react-router";
import { ChevronsUpDownIcon, GitBranchIcon, GitBranchPlusIcon, LayoutGridIcon, MoreHorizontalIcon, Settings2Icon } from "lucide-react";
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
import { buttonVariants } from "#/components/ui/button-variants";
import { cn } from "#/lib/utils";
import { Popover, PopoverContent, PopoverTitle, PopoverTrigger } from "#/components/ui/popover";
import { Command, CommandGroup, CommandItem, CommandList, CommandSeparator } from "#/components/ui/command";
import { Skeleton } from "#/components/ui/skeleton";
import { environmentsQuery, projectsQuery, useCachedStoreView } from "#/modules/config-store/store-view.queries";
import { storeEnvironmentNotes, storeEnvironmentTree } from "#/modules/config-store/store-workspace";

type EnvironmentScope = Extract<DashboardScope, { kind: "environment" }>;

/**
 * `project / environment`, each crumb a switcher; places other than Architecture add their name. A Branch reads
 * `project / parent ⑂ branch`, and its Parent's crumb opens the Parent.
 */
/**
 * On phones the path keeps its last two crumbs; the rest move into a "…" menu so the bar never wraps.
 * `branchAt` marks the crumb a Branch starts at: ⑂ separates it from its Parent instead of "/".
 */
export function Crumbs({ items, branchAt }: { items: ReactNode[]; branchAt?: number }) {
  const collapsed = items.slice(0, -2);
  // The bar outlives the page, and the crumbs in "…" navigate: each page starts it closed.
  const pathname = useLocation({ select: (location) => location.pathname });
  return (
    <Breadcrumb aria-label="Breadcrumb" className="min-w-0">
      <BreadcrumbList className="flex-nowrap">
        {collapsed.length ? <>
          <BreadcrumbItem className="min-wf-nav:hidden">
            <Popover key={pathname}>
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
            {index === branchAt ? <GitBranchIcon /> : "/"}
          </BreadcrumbSeparator> : null}
          <BreadcrumbItem className={cn("min-w-0 *:max-w-full *:truncate", index < collapsed.length && phonesHidden)}>{item}</BreadcrumbItem>
        </Fragment>)}
      </BreadcrumbList>
    </Breadcrumb>
  );
}

const phonesHidden = "hidden min-wf-nav:inline-flex";

export function CrumbTrigger({ label, name, current, className }: { label: string; name: string; current?: boolean; className?: string }) {
  return (
    <PopoverTrigger render={<Button variant={current ? "outline" : "ghost"} size="sm"
      aria-label={`${label}: ${name}`} title={name} className={cn("min-w-0", className)} />}>
      <span className="truncate">{name}</span>
      <ChevronsUpDownIcon data-icon="inline-end" />
    </PopoverTrigger>
  );
}

export function SwitcherLoading({ isPending, retry, children }: { isPending: boolean; retry?: () => void; children: ReactNode }) {
  if (isPending) return (
    <div role="status" aria-label="Loading" className="flex flex-col gap-2 p-2">
      <Skeleton className="h-7 w-full" /><Skeleton className="h-7 w-full" />
    </div>
  );
  if (retry) return <Button variant="ghost" onClick={retry}>Could not load projects. Retry</Button>;
  return children;
}

/**
 * `project / environment`, each crumb a switcher; places other than Architecture add their name. A Branch reads
 * `project / parent ⑂ branch`, and its Parent's crumb opens the Parent.
 */
export function EnvironmentCrumbs({ scope }: { scope: EnvironmentScope }) {
  const section = useDashboardSection();
  const environments = useStoreEnvironments(scope);
  const parent = environments.data.find((row) => row.name === scope.environmentSlug)?.parent;
  return <Crumbs branchAt={parent ? 2 : undefined} items={[
    <ProjectCrumb key="project" scope={scope} />,
    ...parent ? [
      <Link key="parent" {...getDashboardDestination({ ...scope, environmentSlug: parent }, section)}
        className={cn(buttonVariants({ variant: "ghost", size: "sm" }), "min-w-0")} title={parent} aria-label={`Parent: ${parent}`}>
        <span className="truncate">{parent}</span>
      </Link>,
    ] : [],
    <EnvironmentCrumb key={`environment:${scope.projectSlug}/${scope.environmentSlug}`} scope={scope} />,
    ...section === "architecture" ? [] : [<BreadcrumbPage key="place" className="px-1 font-semibold">{getDashboardSectionLabel(section)}</BreadcrumbPage>],
  ]} />;
}

/** Chrome never waits on a Store view: pending until it's read, with a retry when it can't be. */
function useStoreEnvironments(scope: EnvironmentScope) {
  const result = useCachedStoreView(scope.organizationSlug, environmentsQuery(scope.projectSlug));
  return { data: result?.ok ? result.value.environments : [], isPending: result === undefined };
}

function ProjectCrumb({ scope }: { scope: EnvironmentScope }) {
  const result = useCachedStoreView(scope.organizationSlug, projectsQuery());
  const projects = result?.ok ? result.value.projects : [];
  const navigate = useNavigate();
  const section = useDashboardSection();
  const [open, setOpen] = useState(false);
  const go = (destination: DashboardDestination) => { setOpen(false); void navigate(destination); };
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <CrumbTrigger label="Project" name={scope.projectSlug} className="min-wf-nav:-ml-2.5" />
      <PopoverContent padding="none" align="start" className="w-[min(20rem,calc(100vw-2rem))]">
        <PopoverTitle className="sr-only">Switch project</PopoverTitle>
        <SwitcherLoading isPending={result === undefined}>
          <Command tabIndex={0} label="Projects" defaultValue={scope.projectSlug}>
            <CommandList className="max-h-[min(20rem,45dvh)]">
              <CommandGroup heading="Projects">
                {projects.map((candidate) => (
                  <CommandItem key={candidate.id} value={candidate.name} data-checked={candidate.name === scope.projectSlug}
                    aria-label={`${candidate.name}, opens ${candidate.default_environment}`}
                    onSelect={() => go(getDashboardDestination({ ...scope, projectSlug: candidate.name, environmentSlug: candidate.default_environment }, section))}>
                    <span className="flex-1 truncate">{candidate.name}</span>
                    <span className="truncate text-muted-foreground">{candidate.default_environment}</span>
                  </CommandItem>
                ))}
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
  const { data: environments, isPending } = useStoreEnvironments(scope);
  const navigate = useNavigate();
  const section = useDashboardSection();
  const [open, setOpen] = useState(false);
  const current = environments.find((row) => row.name === scope.environmentSlug);
  const { organizationSlug, projectSlug, environmentSlug } = scope;
  const go = (to: () => Promise<void>) => { setOpen(false); void to(); };
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <CrumbTrigger label="Environment" name={scope.environmentSlug} current />
      <PopoverContent padding="none" align="start" className="w-[min(20rem,calc(100vw-2rem))]">
        <PopoverTitle className="sr-only">Switch environment</PopoverTitle>
        <SwitcherLoading isPending={isPending}>
          <Command tabIndex={0} label="Environments" defaultValue={scope.environmentSlug}>
            <CommandList className="max-h-[min(20rem,45dvh)]">
              <CommandGroup heading="Environments">
                {storeEnvironmentTree(environments).map(({ environment, depth }) => {
                  const notes = storeEnvironmentNotes(environment);
                  return (
                    <CommandItem key={environment.id} value={environment.name} data-checked={environment.name === scope.environmentSlug}
                      aria-label={[environment.name, environment.parent && `branch of ${environment.parent}`, ...notes].filter(Boolean).join(", ")}
                      onSelect={() => go(() => navigate(getDashboardDestination({ ...scope, environmentSlug: environment.name }, section)))}>
                      <BranchIndent depth={depth} />
                      <span className="flex-1 truncate">{environment.name}</span>
                      {notes.length > 0 && <span className="shrink-0 text-muted-foreground">{notes.join(" · ")}</span>}
                    </CommandItem>
                  );
                })}
              </CommandGroup>
              <CommandSeparator />
              <CommandGroup>
                {current && <CommandItem value="new-branch" onSelect={() => go(() => navigate({ to: ENVIRONMENT_NEW_BRANCH_ROUTE_TO,
                  params: { organizationSlug, projectSlug, environmentSlug } }))}>
                  <GitBranchPlusIcon />New branch of {current.name}
                </CommandItem>}
                <CommandItem value="manage-environments" onSelect={() => go(() => navigate({ to: "/cloud/$organizationSlug/$projectSlug/$environmentSlug/settings",
                  params: { organizationSlug, projectSlug, environmentSlug }, search: { scope: "project" } }))}>
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
