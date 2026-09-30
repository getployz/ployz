import { useState } from "react";
import type { SetupCommand } from "@ployz/sdk";
import { Link, useNavigate } from "@tanstack/react-router";
import { ChevronRightIcon, GitBranchPlusIcon, PlusIcon } from "lucide-react";
import { BranchIndent } from "#/components/environment-tree";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Field, FieldDescription, FieldLabel } from "#/components/ui/field";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { environmentsQuery, requireView, servicesQuery, useStoreView } from "#/modules/config-store/store-view.queries";
import { SetupCommandsField, useSavedSetupCommands } from "./new-branch/SetupCommandsField";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { storeEnvironmentNotes, storeEnvironmentTree } from "#/modules/config-store/store-workspace";
import { StoreTeardownSection } from "#/routes/_protected/cloud/$organizationSlug/-components/store-teardown-section";
import { CreateEnvironmentDialog } from "./create-environment-dialog";
import { ENVIRONMENT_NEW_BRANCH_ROUTE_TO } from "./environment-route-paths";
import { StorePrEnvironmentsSection } from "./store-pr-environments-section";

type Place = { organizationSlug: string; projectSlug: string; environmentSlug: string };

/** An Environment's settings over the Config Store: deleting it, once it is neither the Default nor a Parent. */
export function StoreEnvironmentSettings({ organizationSlug, projectSlug, environmentSlug }: Place) {
  const navigate = useNavigate();
  const { environments } = requireView(useStoreView(organizationSlug, environmentsQuery(projectSlug)));
  const environment = environments.find((row) => row.name === environmentSlug);
  const branches = environments.filter((row) => row.parent === environmentSlug).map((row) => row.name);
  const disabledReason = environment?.default
    ? `${environmentSlug} is ${projectSlug}'s default environment. Make another the default to delete it, or delete the project.`
    : branches.length > 0 ? `Delete its branches first: ${branches.join(", ")}.` : undefined;
  return (
    <>
    <StoreBranchSetup organizationSlug={organizationSlug} projectSlug={projectSlug} environmentSlug={environmentSlug}
      saved={environment?.branch_setup ?? []} />
    <StoreTeardownSection
      organizationSlug={organizationSlug}
      target={{ project: projectSlug, environment: environmentSlug }}
      environments={environments}
      name={environmentSlug}
      place={`${projectSlug}/${environmentSlug}`}
      title="Delete this environment"
      description="Its services and data go with it."
      actionLabel="Delete environment"
      disabledReason={disabledReason}
      headingId="environment-teardown-heading"
      // The project opens its Default Environment, which this isn't.
      onCompleted={() => void navigate({ to: "/cloud/$organizationSlug/$projectSlug", params: { organizationSlug, projectSlug }, replace: true })}
    />
    </>
  );
}

/**
 * What a new Branch of this Environment runs once, before its services start, when it names no Setup Commands of its
 * own: seeds its fresh data. Saved at once, never staged.
 */
function StoreBranchSetup({ organizationSlug, projectSlug, environmentSlug, saved }: Place & { saved: SetupCommand[] }) {
  const writer = useStoreWriter(organizationSlug);
  const environment = { project: projectSlug, environment: environmentSlug };
  const services = requireView(useStoreView(organizationSlug, servicesQuery(environment))).services
    .filter((service) => service.change !== "delete").map((service) => ({ lineageId: service.name, name: service.name }));
  const setup = useSavedSetupCommands(saved.map((command) => ({ lineageId: command.service, command: command.command })),
    (whole) => writer.commit({ command: "set_branch_setup", environment,
      setup: whole.map((command) => ({ service: command.lineageId, command: command.command })) }));
  return (
    <section aria-labelledby="branch-setup-heading" className="flex flex-col gap-4">
      <div>
        <h2 id="branch-setup-heading" className="text-lg font-semibold">Branches of {environmentSlug}</h2>
        <p className="text-sm text-muted-foreground">
          Setup commands a new branch runs once, in its own copies, before they start: use them to seed fresh data.
        </p>
      </div>
      {services.length ? (
        <SetupCommandsField id="environment-branch-setup" commands={setup.commands} services={services}
          onChange={setup.onChange} onBlur={setup.onBlur} />
      ) : <p className="text-sm text-muted-foreground">Add a service first; commands run in one.</p>}
    </section>
  );
}

/** A Project's settings over the Config Store: its Default Environment, its Environments, and deleting it. */
export function StoreProjectSettings({ organizationSlug, projectSlug, environmentSlug }: Place) {
  const navigate = useNavigate();
  const writer = useStoreWriter(organizationSlug);
  const [creating, setCreating] = useState(false);
  const { environments } = requireView(useStoreView(organizationSlug, environmentsQuery(projectSlug)));
  const defaultEnvironment = environments.find((row) => row.default);
  return (
    <>
      <section aria-labelledby="project-heading" className="flex flex-col gap-4">
        <h2 id="project-heading" className="text-lg font-semibold">{projectSlug}</h2>
        <Field>
          <FieldLabel htmlFor="default-environment">Default environment</FieldLabel>
          <Select value={defaultEnvironment?.name ?? null} onValueChange={(next) => {
            // Refetched views show it; a refusal toasts.
            if (next && next !== defaultEnvironment?.name) {
              writer.commit({ command: "set_default_environment", environment: { project: projectSlug, environment: next } });
            }
          }}>
            <SelectTrigger id="default-environment" className="w-full sm:max-w-sm">
              <SelectValue>{defaultEnvironment?.name}</SelectValue>
            </SelectTrigger>
            <SelectContent>
              <SelectGroup>
                {environments.map((environment) => (
                  <SelectItem key={environment.id} value={environment.name} label={environment.name}>{environment.name}</SelectItem>
                ))}
              </SelectGroup>
            </SelectContent>
          </Select>
          <FieldDescription>Where {projectSlug} opens, for everyone in this organization.</FieldDescription>
        </Field>
      </section>
      <section aria-labelledby="project-environments-heading" className="flex flex-col gap-4">
        <div className="flex flex-wrap items-center justify-between gap-4">
          <h2 id="project-environments-heading" className="text-base font-semibold">Environments</h2>
          <div className="flex gap-2">
            <Button variant="outline" nativeButton={false} render={<Link to={ENVIRONMENT_NEW_BRANCH_ROUTE_TO}
              params={{ organizationSlug, projectSlug, environmentSlug }} />}>
              <GitBranchPlusIcon data-icon="inline-start" />New branch
            </Button>
            <Button variant="outline" onClick={() => setCreating(true)}><PlusIcon data-icon="inline-start" />New environment</Button>
          </div>
        </div>
        <ItemGroup className="gap-2">
          {storeEnvironmentTree(environments).map(({ environment, depth }) => {
            // The Default chip says "default" already.
            const notes = storeEnvironmentNotes(environment).filter((note) => note !== "default");
            return (
              <Item key={environment.id} variant="outline" size="sm" render={
                <Link to="/cloud/$organizationSlug/$projectSlug/$environmentSlug"
                  params={{ organizationSlug, projectSlug, environmentSlug: environment.name }} />
              }>
                <ItemContent className="min-w-0">
                  <ItemTitle className="min-w-0">
                    <BranchIndent depth={depth} />
                    <span className="truncate">{environment.name}</span>
                    {environment.parent && <span className="sr-only">, branch of {environment.parent}</span>}
                    {environment.default && <Badge variant="secondary">Default</Badge>}
                  </ItemTitle>
                  {notes.length > 0 && <ItemDescription className="truncate">{notes.join(" · ")}</ItemDescription>}
                </ItemContent>
                <ItemActions><ChevronRightIcon className="size-4 text-muted-foreground" /></ItemActions>
              </Item>
            );
          })}
        </ItemGroup>
      </section>
      <StorePrEnvironmentsSection organizationSlug={organizationSlug} projectSlug={projectSlug} environmentSlug={environmentSlug} />
      <StoreTeardownSection
        organizationSlug={organizationSlug}
        target={{ project: projectSlug, environment: null }}
        environments={environments}
        name={projectSlug}
        place={projectSlug}
        title="Delete this project"
        description="Its environments, services and data go with it."
        actionLabel="Delete project"
        headingId="project-teardown-heading"
        onCompleted={() => void navigate({ to: "/cloud/$organizationSlug/~", params: { organizationSlug }, replace: true })}
      />
      {creating && <CreateEnvironmentDialog onOpenChange={setCreating} organizationSlug={organizationSlug} projectSlug={projectSlug} />}
    </>
  );
}
