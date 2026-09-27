import { useState, type ReactNode } from "react";
import { useMutation } from "@tanstack/react-query";
import { useNavigate, useRouter } from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import { ChevronsUpDownIcon, LayoutGridIcon, MoreHorizontalIcon, PlusIcon, Settings2Icon } from "lucide-react";
import { getEnvironmentsCollection, getEnvironmentSummariesCollection, environmentSummary } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import {
  getDashboardDestination,
  type DashboardDestination,
  type DashboardScope,
  type DashboardSection,
} from "#/components/dashboard-navigation-model";
import { useDashboardSection } from "#/components/use-dashboard-section";
import { Breadcrumb, BreadcrumbItem, BreadcrumbList, BreadcrumbSeparator } from "#/components/ui/breadcrumb";
import { Button } from "#/components/ui/button";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "#/components/ui/dialog";
import { Field, FieldError, FieldGroup, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { Popover, PopoverContent, PopoverTitle, PopoverTrigger } from "#/components/ui/popover";
import { Command, CommandGroup, CommandItem, CommandList, CommandSeparator } from "#/components/ui/command";
import { Skeleton } from "#/components/ui/skeleton";
import { Spinner } from "#/components/ui/spinner";
import { createEnvironmentServerFn } from "#/modules/environment-design/workspace-functions";
import { findEnvironment, useWorkspace } from "#/modules/environment-design/workspace.queries";
import DashboardAccountMenu from "#/routes/_protected/cloud/$organizationSlug/_org/-components/DashboardAccountMenu";

type EnvironmentScope = Extract<DashboardScope, { kind: "environment" }>;

/** The Environment page's top bar: where you are, each crumb a switcher. */
export function EnvironmentTopBar({ scope }: { scope: EnvironmentScope }) {
  return (
    <header className="flex h-12 shrink-0 items-center gap-2 border-b bg-background px-3 min-wf-nav:px-4">
      <EnvironmentCrumbs scope={scope} />
      <div className="ml-auto min-wf-nav:hidden"><DashboardAccountMenu /></div>
    </header>
  );
}

export function EnvironmentCrumbs({ scope }: { scope: EnvironmentScope }) {
  return <Crumbs items={[
    <ProjectCrumb key="project" scope={scope} />,
    <EnvironmentCrumb key="environment" scope={scope} />,
  ]} />;
}

/** On phones the path keeps its last two crumbs; the rest move into a "…" menu so the bar never wraps. */
export function Crumbs({ items }: { items: ReactNode[] }) {
  const collapsed = items.slice(0, -2);
  return (
    <Breadcrumb aria-label="Breadcrumb" className="min-w-0">
      <BreadcrumbList className="flex-nowrap">
        {collapsed.length ? <>
          <BreadcrumbItem className="min-wf-nav:hidden">
            <Popover>
              <PopoverTrigger render={<Button variant="ghost" size="icon-sm" aria-label="More breadcrumbs" />}>
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
        {items.map((item, index) => {
          const hiddenOnPhones = index < collapsed.length ? "hidden min-wf-nav:inline-flex" : undefined;
          return <Crumb key={index} className={hiddenOnPhones} separator={index > 0}>{item}</Crumb>;
        })}
      </BreadcrumbList>
    </Breadcrumb>
  );
}

function Crumb({ className, separator, children }: { className?: string; separator: boolean; children: ReactNode }) {
  return <>
    {separator ? <BreadcrumbSeparator className={className}>/</BreadcrumbSeparator> : null}
    <BreadcrumbItem className={className ?? "min-w-0"}>{children}</BreadcrumbItem>
  </>;
}

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
                <CommandItem value="all-projects" onSelect={() => go(getDashboardDestination({ kind: "all", organizationSlug: scope.organizationSlug }, "overview"))}>
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
  const { projects, environments, isPending, isError, refetch } = useWorkspace(scope.organizationSlug);
  const navigate = useNavigate();
  const section = useDashboardSection();
  const [open, setOpen] = useState(false);
  const [creating, setCreating] = useState(false);
  const project = projects.find((candidate) => candidate.slug === scope.projectSlug);
  const current = findEnvironment(projects, environments, scope);
  const projectEnvironments = environments.filter((environment) => environment.projectId === project?.id);
  return (
    <>
      <Popover open={open} onOpenChange={setOpen}>
        <CrumbTrigger label="Environment" name={current?.name ?? scope.environmentSlug} current />
        <PopoverContent padding="none" align="start" className="w-[min(18rem,calc(100vw-2rem))]">
          <PopoverTitle className="sr-only">Switch environment</PopoverTitle>
          <SwitcherLoading isPending={isPending} retry={isError ? () => void refetch() : undefined}>
            <Command tabIndex={0} label="Environments" defaultValue={current?.id}>
              <CommandList className="max-h-[min(20rem,45dvh)]">
                <CommandGroup heading="Environments">
                  {projectEnvironments.map((environment) => (
                    <CommandItem key={environment.id} value={environment.id} keywords={[environment.name]}
                      data-checked={environment.id === current?.id}
                      aria-label={environment.id === project?.resolvedEnvironment?.id ? `${environment.name}, default` : environment.name}
                      onSelect={() => {
                        setOpen(false);
                        void navigate(getDashboardDestination({ ...scope, environmentSlug: environment.namespace }, section));
                      }}>
                      <span className="flex-1 truncate">{environment.name}</span>
                      {environment.id === project?.resolvedEnvironment?.id && <span className="text-muted-foreground">Default</span>}
                    </CommandItem>
                  ))}
                </CommandGroup>
                <CommandSeparator />
                <CommandGroup>
                  <CommandItem value="new-environment" onSelect={() => { setOpen(false); setCreating(true); }}>
                    <PlusIcon />New environment
                  </CommandItem>
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
      {creating && <CreateEnvironmentDialog onOpenChange={setCreating}
        organizationSlug={scope.organizationSlug} projectSlug={scope.projectSlug} section={section} />}
    </>
  );
}

function CreateEnvironmentDialog({
  onOpenChange,
  organizationSlug,
  projectSlug,
  section,
}: {
  onOpenChange: (open: boolean) => void;
  organizationSlug: string;
  projectSlug: string;
  section: DashboardSection;
}) {
  const collectionScope = useCollectionScope();
  const [name, setName] = useState("");
  const router = useRouter();
  const navigate = useNavigate();
  const createEnvironment = useServerFn(createEnvironmentServerFn);
  const mutation = useMutation({
    mutationFn: (input: {
      organizationSlug: string;
      projectSlug: string;
      name: string;
      locationKey: string | undefined;
    }) =>
      createEnvironment({
        data: {
          organizationSlug: input.organizationSlug,
          projectSlug: input.projectSlug,
          name: input.name,
        },
      }),
    onSuccess: async (receipt, input) => {
      await getEnvironmentsCollection(input.organizationSlug, collectionScope).writeCommitted(receipt.data);
      await getEnvironmentSummariesCollection(input.organizationSlug, collectionScope).writeCommitted(environmentSummary(receipt.data));
      // A completed creation still belongs to its original scope after navigation.
      if (router.state.location.state.key !== input.locationKey) return;
      onOpenChange(false);
      await navigate(
        getDashboardDestination(
          {
            kind: "environment",
            organizationSlug: input.organizationSlug,
            projectSlug: input.projectSlug,
            environmentSlug: receipt.data.namespace,
          },
          section,
        ),
      );
    },
  });

  return (
    <Dialog open onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Add environment</DialogTitle>
          <DialogDescription>
            Create an empty environment with no services or variables.
          </DialogDescription>
        </DialogHeader>
        <form
          onSubmit={(event) => {
            event.preventDefault();
            if (name.trim() && !mutation.isPending)
              mutation.mutate({
                organizationSlug,
                projectSlug,
                name: name.trim(),
                locationKey: router.state.location.state.key,
              });
          }}
        >
          <FieldGroup>
            <Field>
              <FieldLabel htmlFor="env-name">Name</FieldLabel>
              <Input
                id="env-name"
                placeholder="staging"
                value={name}
                onChange={(event) => setName(event.target.value)}
                autoFocus
              />
            </Field>
            {mutation.isError && (
              <FieldError>{mutation.error.message}</FieldError>
            )}
          </FieldGroup>
          <DialogFooter className="mt-4">
            <DialogClose render={<Button variant="outline" />}>
              Cancel
            </DialogClose>
            <Button type="submit" disabled={!name.trim() || mutation.isPending}>
              {mutation.isPending && <Spinner data-icon="inline-start" />}
              Add environment
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
