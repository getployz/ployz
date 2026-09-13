import { useCollectionScope } from "#/collections/use-collection-scope";
import { type ReactElement, type ReactNode, useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import {
  Link,
  useNavigate,
  useParams,
} from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import {
  CheckIcon,
  ChevronDownIcon,
  PlusIcon,
  SlashIcon,
} from "lucide-react";
import { getEnvironmentsCollection } from "#/collections/collections";
import {
  getDashboardDestination,
  getDashboardProjectDestination,
  type DashboardSection,
} from "#/components/dashboard-navigation-model";
import { useDashboardSection } from "#/components/use-dashboard-section";
import { Button } from "#/components/ui/button";
import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbLink,
  BreadcrumbList,
  BreadcrumbSeparator,
} from "#/components/ui/breadcrumb";
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
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "#/components/ui/dropdown-menu";
import { Field, FieldLabel } from "#/components/ui/field";
import { FieldGroup } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { Spinner } from "#/components/ui/spinner";
import { createEnvironmentServerFn } from "#/modules/environment-design/workspace-functions";
import {
  environmentListQueryOptions,
  organizationStateQueryOptions,
  projectListQueryOptions,
} from "#/modules/environment-design/workspace-queries";
import { Route as NewProjectRoute } from "#/routes/_protected/cloud/$organizationSlug/_project/new";

type SwitcherItem = {
  key: string;
  label: string;
  active: boolean;
  disabled?: boolean;
  render?: ReactElement;
};

type ProjectScopeSource = {
  id: string;
  name: string;
  slug: string;
  resolvedEnvironment: { namespace: string } | null;
};

function createProjectScopeItems(
  projects: ProjectScopeSource[],
  projectSlug?: string,
) {
  return projects.map((project) => ({
    key: project.id,
    label: project.name,
    projectSlug: project.slug,
    environmentSlug: project.resolvedEnvironment?.namespace,
    active: project.slug === projectSlug,
  }));
}

type SwitcherAction = {
  key: string;
  label: string;
  icon: ReactNode;
} & (
  | { render: ReactElement; onClick?: never }
  | { onClick: () => void; render?: never }
);

function Switcher({
  label,
  icon,
  items,
  actions,
  triggerVariant = "ghost",
}: {
  label: string;
  icon?: ReactNode;
  items: SwitcherItem[];
  actions?: SwitcherAction[];
  triggerVariant?: "ghost" | "outline";
}) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <Button
            variant={triggerVariant}
            size="sm"
            className="max-w-48 min-w-0"
          />
        }
      >
        {icon}
        <span className="truncate">{label}</span>
        <ChevronDownIcon data-icon="inline-end" />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-auto">
        <DropdownMenuGroup>
          {items.map((item) => (
            <DropdownMenuItem
              key={item.key}
              disabled={item.disabled}
              render={item.render}
            >
              {item.active ? (
                <CheckIcon />
              ) : (
                <span className="size-4" />
              )}
              {item.label}
            </DropdownMenuItem>
          ))}
        </DropdownMenuGroup>
        {actions && actions.length > 0 ? (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuGroup>
              {actions.map((action) => (
                <DropdownMenuItem
                  key={action.key}
                  {...("render" in action
                    ? { render: action.render }
                    : { onClick: action.onClick })}
                >
                  {action.icon}
                  {action.label}
                </DropdownMenuItem>
              ))}
            </DropdownMenuGroup>
          </>
        ) : null}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

export function ProjectSwitcher({
  organizationSlug,
  projectSlug,
  section,
  triggerLabel,
  triggerVariant,
}: {
  organizationSlug: string;
  projectSlug?: string;
  section: DashboardSection;
  triggerLabel?: string;
  triggerVariant?: "ghost" | "outline";
}) {
  const { data: projects = [] } = useQuery(
    projectListQueryOptions(organizationSlug),
  );
  const projectScopes = createProjectScopeItems(projects, projectSlug);
  const activeProject = projectScopes.find((project) => project.active);
  const allProjectsDestination = getDashboardDestination(
    { kind: "all", organizationSlug },
    section,
  );

  return (
    <Switcher
      label={triggerLabel ?? activeProject?.label ?? "All projects"}
      triggerVariant={triggerVariant}
      items={[
        {
          key: "all-projects",
          label: "All projects",
          active: !projectSlug,
          render: (
            <Link
              to={allProjectsDestination.to}
              params={allProjectsDestination.params}
            />
          ),
        },
        ...projectScopes.map((project) => {
          const destination = project.environmentSlug
            ? getDashboardProjectDestination(
                {
                  kind: "environment",
                  organizationSlug,
                  projectSlug: project.projectSlug,
                  environmentSlug: project.environmentSlug,
                },
                section,
              )
            : null;

          return {
            key: project.key,
            label: project.label,
            active: project.active,
            disabled: !destination,
            render: destination ? (
              <Link to={destination.to} params={destination.params} />
            ) : undefined,
          };
        }),
      ]}
      actions={[
        {
          key: "new-project",
          label: "Project",
          icon: <PlusIcon />,
          render: (
            <Link to={NewProjectRoute.to} params={{ organizationSlug }} />
          ),
        },
      ]}
    />
  );
}

function CreateEnvironmentDialog({
  open,
  onOpenChange,
  organizationSlug,
  projectSlug,
  section,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  organizationSlug: string;
  projectSlug: string;
  section: DashboardSection;
}) {
  const collectionScope = useCollectionScope();
  const [name, setName] = useState("");
  const navigate = useNavigate();
  const createEnvironment = useServerFn(createEnvironmentServerFn);
  const mutation = useMutation({
    mutationFn: () =>
      createEnvironment({
        data: { organizationSlug, projectSlug, name },
      }),
    onSuccess: async (receipt) => {
      await getEnvironmentsCollection(organizationSlug, collectionScope).writeCommitted(receipt.data);
      onOpenChange(false);
      setName("");
      const destination = getDashboardProjectDestination(
        {
          kind: "environment",
          organizationSlug,
          projectSlug,
          environmentSlug: receipt.data.namespace,
        },
        section,
      );
      await navigate({
        to: destination.to,
        params: destination.params,
        search: (prev) => prev,
      });
    },
  });

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Add environment</DialogTitle>
          <DialogDescription>
            Create an empty environment with no services or variables.
          </DialogDescription>
        </DialogHeader>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            mutation.mutate();
          }}
        >
          <FieldGroup>
            <Field>
              <FieldLabel htmlFor="env-name">Name</FieldLabel>
              <Input
                id="env-name"
                placeholder="staging"
                value={name}
                onChange={(e) => setName(e.target.value)}
                autoFocus
              />
            </Field>
          </FieldGroup>
          <DialogFooter className="mt-4">
            <DialogClose render={<Button variant="outline" />}>
              Cancel
            </DialogClose>
            <Button type="submit" disabled={!name.trim() || mutation.isPending}>
              {mutation.isPending ? <Spinner /> : null}
              Add environment
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function EnvironmentSwitcher({
  organizationSlug,
  projectSlug,
  environmentSlug,
  section,
}: {
  organizationSlug: string;
  projectSlug: string;
  environmentSlug: string;
  section: DashboardSection;
}) {
  const [createOpen, setCreateOpen] = useState(false);
  const { data: environments = [] } = useQuery(
    environmentListQueryOptions(organizationSlug, projectSlug),
  );
  const activeEnvironment = environments.find(
    (e) => e.namespace === environmentSlug,
  );

  return (
    <>
      <Switcher
        label={activeEnvironment?.name ?? environmentSlug}
        items={environments.map((env) => {
          const destination = getDashboardProjectDestination(
            {
              kind: "environment",
              organizationSlug,
              projectSlug,
              environmentSlug: env.namespace,
            },
            section,
          );

          return {
            key: env.id,
            label: env.name,
            active: env.namespace === environmentSlug,
            render: (
              <Link to={destination.to} params={destination.params} />
            ),
          };
        })}
        actions={[
          {
            key: "new-environment",
            label: "Environment",
            icon: <PlusIcon />,
            onClick: () => setCreateOpen(true),
          },
        ]}
      />
      <CreateEnvironmentDialog
        open={createOpen}
        onOpenChange={setCreateOpen}
        organizationSlug={organizationSlug}
        projectSlug={projectSlug}
        section={section}
      />
    </>
  );
}

export function NavigationSwitcher({
  projection = "desktop",
}: {
  projection?: "desktop" | "mobile";
}) {
  const params = useParams({ strict: false });
  const section = useDashboardSection();

  const { organizationSlug, projectSlug, environmentSlug } = params;
  const { data: organizationState } = useQuery(
    organizationStateQueryOptions(organizationSlug),
  );

  if (!organizationSlug) return null;

  const organizationLabel =
    organizationState?.activeOrganization?.name ?? organizationSlug;
  const showOrganization = projection === "desktop";
  const overviewDestination = getDashboardDestination(
    { kind: "all", organizationSlug },
    "overview",
  );

  return (
    <Breadcrumb className="min-w-0">
      <BreadcrumbList className="flex-nowrap overflow-hidden">
        {showOrganization ? (
          <BreadcrumbItem className="min-w-0">
            <BreadcrumbLink
              render={
                <Link
                  to={overviewDestination.to}
                  params={overviewDestination.params}
                />
              }
              className="max-w-40 truncate"
            >
              {organizationLabel}
            </BreadcrumbLink>
          </BreadcrumbItem>
        ) : null}
        {showOrganization ? (
          <BreadcrumbSeparator>
            <SlashIcon />
          </BreadcrumbSeparator>
        ) : null}
        <BreadcrumbItem className="min-w-0">
          <ProjectSwitcher
            organizationSlug={organizationSlug}
            projectSlug={projectSlug && environmentSlug ? projectSlug : undefined}
            section={section}
          />
        </BreadcrumbItem>
        {projectSlug && environmentSlug ? (
          <>
            <BreadcrumbSeparator>
              <SlashIcon />
            </BreadcrumbSeparator>
            <BreadcrumbItem className="min-w-0">
              <EnvironmentSwitcher
                organizationSlug={organizationSlug}
                projectSlug={projectSlug}
                environmentSlug={environmentSlug}
                section={section}
              />
            </BreadcrumbItem>
          </>
        ) : null}
      </BreadcrumbList>
    </Breadcrumb>
  );
}
