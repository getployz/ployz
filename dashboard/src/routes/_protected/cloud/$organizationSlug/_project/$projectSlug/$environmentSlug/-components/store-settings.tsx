import { useState } from "react";
import type { SetupCommand } from "@ployz/sdk";
import { Link, useNavigate } from "@tanstack/react-router";
import { ChevronRightIcon, GitBranchPlusIcon, PlusIcon } from "lucide-react";
import { BranchIndent } from "#/components/environment-tree";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Field, FieldContent, FieldDescription, FieldError, FieldLabel } from "#/components/ui/field";
import { SettingsSection } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { environmentsQuery, servicesQuery, useStoreView } from "#/modules/config-store/store-view.queries";
import { SetupCommandsField, useSavedSetupCommands } from "./new-branch/SetupCommandsField";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { dnsLabelError } from "#/modules/config-store/store-services";
import { Input } from "#/components/ui/input";
import { storeEnvironmentNotes, storeEnvironmentTree } from "#/modules/config-store/store-workspace";
import { StoreTeardownSection } from "#/routes/_protected/cloud/$organizationSlug/-components/store-teardown-section";
import { CreateEnvironmentDialog } from "./create-environment-dialog";
import { ENVIRONMENT_NEW_BRANCH_ROUTE_TO } from "./environment-route-paths";
import { StorePrEnvironmentsSection } from "./store-pr-environments-section";

type Place = { organizationSlug: string; projectSlug: string; environmentSlug: string };

/** An Environment's settings over the Config Store: deleting it, once it is neither the Default nor a Parent. */
export function StoreEnvironmentSettings({ organizationSlug, projectSlug, environmentSlug }: Place) {
  const navigate = useNavigate();
  const listing = useStoreView(organizationSlug, environmentsQuery(projectSlug));
  // Deleted (here, or elsewhere) while open: nothing to show while the page moves on.
  if (!listing.ok) return null;
  const { environments } = listing.value;
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
  const listed = useStoreView(organizationSlug, servicesQuery(environment));
  const services = (listed.ok ? listed.value.services : [])
    .filter((service) => service.change !== "delete").map((service) => ({ lineageId: service.name, name: service.name }));
  const setup = useSavedSetupCommands(saved.map((command) => ({ lineageId: command.service, command: command.command })),
    (whole) => writer.commit({ command: "set_branch_setup", environment,
      setup: whole.map((command) => ({ service: command.lineageId, command: command.command })) }));
  return (
    <SettingsSection id="branch-setup" title={`Branches of ${environmentSlug}`}
      description="Runs once in each new branch.">
      {services.length ? (
        <SetupCommandsField id="environment-branch-setup" commands={setup.commands} services={services}
          onChange={setup.onChange} onBlur={setup.onBlur} />
      ) : <FieldDescription>Add a service first.</FieldDescription>}
    </SettingsSection>
  );
}

/** A Project's settings over the Config Store: its Default Environment, its Environments, and deleting it. */
/** A Project's name: one DNS label, as the Store checks it; renaming it changes its URLs. */
function ProjectNameField({ project, onRename }: { project: string; onRename: (name: string) => void }) {
  const [draft, setDraft] = useState(project);
  const name = draft.trim();
  const error = name === project ? null : dnsLabelError(name);
  const rename = () => { if (name !== project && !error) onRename(name); };
  return (
    <Field orientation="responsive" data-invalid={error ? true : undefined}>
      <FieldContent>
        <FieldLabel htmlFor="project-name">Name</FieldLabel>
        {error ? <FieldError>{error}</FieldError> : <FieldDescription>Changes the project URL.</FieldDescription>}
      </FieldContent>
      <div className="flex gap-2 @md/field-group:shrink-0 @md/field-group:basis-80">
        <Input id="project-name" value={draft} aria-invalid={error ? true : undefined} onChange={(event) => setDraft(event.target.value)}
          onKeyDown={(event) => { if (event.key === "Enter") rename(); }} />
        <Button variant="outline" disabled={name === project || error !== null} onClick={rename}>Rename</Button>
      </div>
    </Field>
  );
}

export function StoreProjectSettings({ organizationSlug, projectSlug, environmentSlug }: Place) {
  const navigate = useNavigate();
  const writer = useStoreWriter(organizationSlug);
  const [creating, setCreating] = useState(false);
  const listing = useStoreView(organizationSlug, environmentsQuery(projectSlug));
  // Deleted while open: nothing to show while the page moves on.
  if (!listing.ok) return null;
  const { environments } = listing.value;
  const defaultEnvironment = environments.find((row) => row.default);
  return (
    <>
      <SettingsSection id="project" title="Project">
        <ProjectNameField project={projectSlug} onRename={(name) => {
          // Awaited: the page opens the renamed Project once the Store has it; a refusal toasts and stays here.
          void writer.commit({ command: "rename_project", project: projectSlug, name }).isPersisted.promise.then(() =>
            navigate({ to: ".", params: (params) => ({ ...params, projectSlug: name }), search: (prev) => prev }), () => undefined);
        }} />
        <Field orientation="responsive">
          <FieldContent>
            <FieldLabel htmlFor="default-environment">Default environment</FieldLabel>
            <FieldDescription>Opens by default.</FieldDescription>
          </FieldContent>
          <Select value={defaultEnvironment?.name ?? null} onValueChange={(next) => {
            // Refetched views show it; a refusal toasts.
            if (next && next !== defaultEnvironment?.name) {
              writer.commit({ command: "set_default_environment", environment: { project: projectSlug, environment: next } });
            }
          }}>
            <SelectTrigger id="default-environment" className="w-full @md/field-group:w-80">
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
        </Field>
      </SettingsSection>
      <SettingsSection id="project-environments" title="Environments" action={<>
        <Button variant="outline" nativeButton={false} render={<Link to={ENVIRONMENT_NEW_BRANCH_ROUTE_TO}
          params={{ organizationSlug, projectSlug, environmentSlug }} />}>
          <GitBranchPlusIcon data-icon="inline-start" />New branch
        </Button>
        <Button variant="outline" onClick={() => setCreating(true)}><PlusIcon data-icon="inline-start" />New environment</Button>
      </>}>
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
      </SettingsSection>
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
