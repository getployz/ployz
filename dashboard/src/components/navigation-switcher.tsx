import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import {
  Link,
  useNavigate,
  useParams,
  useRouter,
} from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import {
  Building2Icon,
  ChevronsUpDownIcon,
  FolderIcon,
  PlusIcon,
} from "lucide-react";
import { getEnvironmentsCollection, getEnvironmentSummariesCollection, environmentSummary } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import {
  getDashboardDestination,
  type DashboardSection,
} from "#/components/dashboard-navigation-model";
import { useDashboardSection } from "#/components/use-dashboard-section";
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
import {
  Field,
  FieldError,
  FieldGroup,
  FieldLabel,
} from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { Popover, PopoverContent, PopoverTitle, PopoverTrigger } from "#/components/ui/popover";
import { Command, CommandEmpty, CommandGroup, CommandItem, CommandList } from "#/components/ui/command";
import { Spinner } from "#/components/ui/spinner";
import { cn } from "#/lib/utils";
import { createEnvironmentServerFn } from "#/modules/environment-design/workspace-functions";
import { findEnvironment,
  organizationStateQueryOptions,
  useWorkspace,
} from "#/modules/environment-design/workspace.queries";

type Projection = "desktop" | "mobile" | "rail";

export function ProjectSwitcher({
  organizationSlug,
  projectSlug,
  environmentSlug,
  section,
  triggerLabel,
  triggerVariant,
  projection = "desktop",
}: {
  organizationSlug: string;
  projectSlug?: string;
  environmentSlug?: string;
  section: DashboardSection;
  triggerLabel?: string;
  triggerVariant?: "ghost" | "outline";
  projection?: Projection;
}) {
  const { projects, environments, isPending, isError, refetch } = useWorkspace(organizationSlug);
  const navigate = useNavigate();
  const [open, setOpen] = useState(false);
  const [browsedProjectSlug, setBrowsedProjectSlug] = useState(projectSlug);
  const [createProject, setCreateProject] = useState<string | null>(null);
  const activeProject = projects.find((project) => project.slug === projectSlug);
  const activeEnvironment = findEnvironment(projects, environments, { projectSlug, environmentSlug });
  const selectedProject = projects.find((project) => project.slug === browsedProjectSlug) ?? activeProject ?? projects[0];
  const projectEnvironments = environments.filter((environment) => environment.projectId === selectedProject?.id);
  const label = triggerLabel ?? (projectSlug ? activeProject?.name ?? projectSlug : "Choose project");
  const suffix = !triggerLabel && projectSlug
    ? activeEnvironment?.name ?? environmentSlug ?? activeProject?.resolvedEnvironment?.name
    : undefined;
  const fullLabel = suffix ? `${label} / ${suffix}` : label;
  const rail = projection === "rail";

  return (
    <>
      <Popover open={open} onOpenChange={(nextOpen) => {
        setOpen(nextOpen);
        if (nextOpen) setBrowsedProjectSlug(projectSlug);
      }}>
        <PopoverTrigger openOnHover={rail} render={
          <Button variant={triggerVariant ?? "ghost"} size={rail ? "icon" : projection === "mobile" ? "sm" : "default"}
            aria-label={`Project and environment: ${fullLabel}`} title={fullLabel}
            className={cn(!rail && "w-full min-w-0 justify-start")} />
        }>
          {projection !== "mobile" && <FolderIcon data-icon="inline-start" />}
          {!rail && <>
            <span className="flex min-w-0 flex-1 items-center gap-1 text-left">
              <span className="min-w-0 truncate">{label}</span>
              {suffix && <><span aria-hidden className="shrink-0 text-muted-foreground">/</span><span className="max-w-[65%] shrink-0 truncate">{suffix}</span></>}
            </span>
            <ChevronsUpDownIcon data-icon="inline-end" />
          </>}
        </PopoverTrigger>
        <PopoverContent padding="none" align="start" side={rail ? "right" : "bottom"} className="w-[min(36rem,calc(100vw-2rem))]">
          <PopoverTitle className="sr-only">Choose project and environment</PopoverTitle>
          {isPending || isError ? (
            <Button variant="ghost" disabled={isPending} onClick={() => void refetch()}>
              {isPending ? "Loading projects…" : "Could not load projects. Retry"}
            </Button>
          ) : <div className="grid grid-cols-2">
            <div className="min-w-0 border-r">
              <Command tabIndex={0} label="Projects" value={selectedProject?.slug ?? ""}
                onValueChange={(value) => { if (projects.some((project) => project.slug === value)) setBrowsedProjectSlug(value); }}>
                <CommandList className="min-h-40 max-h-[min(20rem,45dvh)]">
                  <CommandEmpty>No projects found</CommandEmpty>
                  <CommandGroup heading="Projects">
                    {projects.map((project) => <CommandItem key={project.id} value={project.slug} keywords={[project.name]}
                      data-checked={project.slug === projectSlug} aria-label={project.name}
                      onSelect={() => {
                        const environment = project.resolvedEnvironment;
                        if (!environment) return;
                        setOpen(false);
                        void navigate(getDashboardDestination({
                          kind: "environment", organizationSlug,
                          projectSlug: project.slug, environmentSlug: environment.namespace
                        }, section));
                      }}>
                      <FolderIcon /><span className="truncate">{project.name}</span>
                    </CommandItem>)}
                  </CommandGroup>
                </CommandList>
              </Command>
            </div>
            <div className="min-w-0">
              <Command key={selectedProject?.id} tabIndex={0} label="Environments"
                defaultValue={projectEnvironments.find((environment) => environment.namespace === (
                  selectedProject?.slug === projectSlug ? environmentSlug : selectedProject?.resolvedEnvironment?.namespace
                ))?.id}>
                <CommandList className="min-h-40 max-h-[min(20rem,45dvh)]">
                  <CommandEmpty>No environments found</CommandEmpty>
                  <CommandGroup heading="Environments">
                    {projectEnvironments.map((environment) => <CommandItem key={environment.id} value={environment.id} keywords={[environment.name]}
                      data-checked={selectedProject?.slug === projectSlug && environment.namespace === environmentSlug}
                      aria-label={environment.name}
                      onSelect={() => {
                        if (!selectedProject) return;
                        setOpen(false);
                        void navigate(getDashboardDestination({
                          kind: "environment", organizationSlug,
                          projectSlug: selectedProject.slug, environmentSlug: environment.namespace
                        }, section));
                      }}>
                      <span className="truncate">{environment.name}</span>
                    </CommandItem>)}
                  </CommandGroup>
                </CommandList>
              </Command>
            </div>
            <div className="min-w-0 border-r border-t px-2 py-1">
              <Button variant="ghost" size="sm" className="w-full justify-start" onClick={() => setOpen(false)}
                render={<Link preload={false} to="/cloud/$organizationSlug/new" params={{ organizationSlug }} search={{}} />}>
                <PlusIcon data-icon="inline-start" /><span className="truncate">New project</span>
              </Button>
            </div>
            <div className="min-w-0 border-t px-2 py-1">
              <Button variant="ghost" size="sm" className="w-full justify-start" disabled={!selectedProject} onClick={() => {
                if (!selectedProject) return;
                setOpen(false);
                setCreateProject(selectedProject.slug);
              }}><PlusIcon data-icon="inline-start" /><span className="truncate">New environment</span></Button>
            </div>
          </div>}

        </PopoverContent>
      </Popover>
      {createProject && <CreateEnvironmentDialog key={`${organizationSlug}/${createProject}`}
        onOpenChange={(nextOpen) => { if (!nextOpen) setCreateProject(null); }}
        organizationSlug={organizationSlug} projectSlug={createProject} section={section} />}
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

export function NavigationSwitcher({
  projection = "desktop",
}: {
  projection?: Projection;
}) {
  const [organizationOpen, setOrganizationOpen] = useState(false);
  const navigate = useNavigate();
  const rail = projection === "rail";
  const { organizationSlug, projectSlug, environmentSlug } = useParams({
    strict: false,
  });
  const section = useDashboardSection();
  const {
    data: organizationState,
    isPending,
    isError,
    refetch,
  } = useQuery(organizationStateQueryOptions(organizationSlug));
  if (!organizationSlug) return null;
  const organizationLabel =
    organizationState?.activeOrganization?.name ?? organizationSlug;

  return (
    <div
      className={cn(
        "flex min-w-0 gap-1",
        projection === "mobile"
          ? "w-full [&>*]:min-w-0 [&>*]:flex-1 [&>*:first-child]:max-w-[30%]"
          : "flex-col",
        projection === "rail" && "items-center",
      )}
    >
      <Popover open={organizationOpen} onOpenChange={setOrganizationOpen}>
        <PopoverTrigger openOnHover={rail} render={
          <Button variant="ghost" size={rail ? "icon" : projection === "mobile" ? "sm" : "default"}
            aria-label={`Organization: ${organizationLabel}`} title={organizationLabel}
            className={cn(!rail && "w-full min-w-0 justify-start")} />
        }>
          {projection !== "mobile" && <Building2Icon data-icon="inline-start" />}
          {!rail && <>
            <span className="min-w-0 flex-1 truncate text-left">{organizationLabel}</span>
            <ChevronsUpDownIcon data-icon="inline-end" />
          </>}
        </PopoverTrigger>
        <PopoverContent padding="none" align="start" side={rail ? "right" : "bottom"}
          className="w-[min(18rem,calc(100vw-2rem))]">
          <PopoverTitle className="sr-only">Choose organization</PopoverTitle>
          <div className="min-w-0">
            <Command tabIndex={0} label="Organizations">
              <CommandList className="max-h-[min(20rem,45dvh)]">
                <CommandGroup heading="Organizations">
                  {organizationState?.organizations.map((organization) => (
                    <CommandItem key={organization.id} value={organization.slug}
                      data-checked={organization.slug === organizationSlug}
                      onSelect={() => {
                        setOrganizationOpen(false);
                        void navigate(getDashboardDestination(
                          { kind: "all", organizationSlug: organization.slug }, section,
                        ));
                      }}>
                      <Building2Icon /><span className="truncate">{organization.name}</span>
                    </CommandItem>
                  ))}
                  {isPending || isError ? (
                    <CommandItem disabled={isPending} onSelect={() => void refetch()}>
                      {isPending ? "Loading organizations…" : "Could not load organizations. Retry"}
                    </CommandItem>
                  ) : null}
                </CommandGroup>
              </CommandList>
            </Command>
          </div>
        </PopoverContent>
      </Popover>
      <ProjectSwitcher
        key={`${organizationSlug}/${projectSlug ?? ""}/${environmentSlug ?? ""}`}
        organizationSlug={organizationSlug}
        projectSlug={projectSlug && environmentSlug ? projectSlug : undefined}
        environmentSlug={environmentSlug}
        section={section}
        projection={projection}
      />
    </div>
  );
}
