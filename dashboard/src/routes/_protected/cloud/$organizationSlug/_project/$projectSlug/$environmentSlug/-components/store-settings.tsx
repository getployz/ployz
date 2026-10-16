import { useState } from "react";
import type { SetupCommand } from "@ployz/sdk";
import { useNavigate } from "@tanstack/react-router";
import { CodeIcon, EllipsisIcon } from "lucide-react";
import { SettingsEmpty, SettingsForm, SettingsList, SettingsListAdd, SettingsListItem } from "#/components/settings/settings-list";
import { SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { ConfirmableInput } from "#/components/stageable/confirmable-input";
import { useCommitHold } from "#/components/stageable/use-commit-hold";
import { Button } from "#/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { Field, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import type { SetupCommand as DraftCommand } from "#/modules/config-store/branch-picks";
import { environmentsQuery, servicesQuery, useStoreView } from "#/modules/config-store/store-view.queries";
import { useSavedSetupCommands } from "./new-branch/SetupCommandsField";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { dnsLabelError } from "#/modules/config-store/store-services";
import { StoreTeardownSection } from "#/routes/_protected/cloud/$organizationSlug/-components/store-teardown-section";
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
  const [editing, setEditing] = useState<number | null>(null);
  const nameOf = (lineage: string) => services.find((service) => service.lineageId === lineage)?.name ?? "a service";
  // A new command runs where the last one does.
  const last = services.find((service) => service.lineageId === setup.commands.at(-1)?.lineageId) ?? services[0];
  return (
    <SettingsSection id="branch-setup" title={`Branches of ${environmentSlug}`}
      description="Runs once in each new branch.">
      {!last ? <SettingsEmpty title="Add a service first." /> : <>
        {setup.commands.length === 0 ? <SettingsEmpty title="No setup commands" /> : (
          <SettingsList label="Setup commands">
            {setup.commands.map((command, index) => (
              <SettingsListItem
                key={index}
                media={<CodeIcon />}
                label={<span className="font-mono">{command.command}</span>}
                hint={<>Runs in {nameOf(command.lineageId)}</>}
                commit="change"
                actions={(
                  <DropdownMenu>
                    <DropdownMenuTrigger render={<Button variant="ghost" size="icon" aria-label={`${command.command} actions`} />}>
                      <EllipsisIcon />
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end">
                      <DropdownMenuItem onClick={() => setEditing(index)}>Edit</DropdownMenuItem>
                      <DropdownMenuItem variant="destructive" onClick={() => setup.onChange(setup.commands.filter((_, i) => i !== index))}>
                        Remove
                      </DropdownMenuItem>
                    </DropdownMenuContent>
                  </DropdownMenu>
                )}
              />
            ))}
          </SettingsList>
        )}
        {editing !== null && setup.commands[editing] ? (
          <SetupCommandForm submitLabel="Save" services={services} initial={setup.commands[editing]} onCancel={() => setEditing(null)}
            onSubmit={(next) => { setup.onChange(setup.commands.map((command, i) => i === editing ? next : command)); setEditing(null); }} />
        ) : (
          <SettingsListAdd label="Add command">
            {(close) => (
              <SetupCommandForm submitLabel="Add" services={services} initial={{ lineageId: last.lineageId, command: "" }} onCancel={close}
                onSubmit={(next) => { setup.onChange([...setup.commands, next]); close(); }} />
            )}
          </SettingsListAdd>
        )}
      </>}
    </SettingsSection>
  );
}

/** One Setup Command as it's added or edited: what to run, and the service it runs in. */
function SetupCommandForm({ submitLabel, services, initial, onSubmit, onCancel }: {
  submitLabel: string;
  services: Array<{ lineageId: string; name: string }>;
  initial: DraftCommand;
  onSubmit: (command: DraftCommand) => void;
  onCancel: () => void;
}) {
  const [command, setCommand] = useState(initial.command);
  const [lineageId, setLineageId] = useState(initial.lineageId);
  return (
    <SettingsForm submitLabel={submitLabel} disabled={!command.trim()} onCancel={onCancel}
      onSubmit={() => onSubmit({ lineageId, command: command.trim() })}>
      <Field>
        <FieldLabel htmlFor="setup-command">Command</FieldLabel>
        <Input id="setup-command" className="font-mono" placeholder="pnpm db:seed" spellCheck={false} autoComplete="off" autoFocus
          value={command} onChange={(event) => setCommand(event.target.value)} />
      </Field>
      <Field>
        <FieldLabel htmlFor="setup-runs-in">Runs in</FieldLabel>
        <Select value={lineageId} onValueChange={(next) => { if (next) setLineageId(next); }}>
          <SelectTrigger id="setup-runs-in">
            <SelectValue>{services.find((service) => service.lineageId === lineageId)?.name}</SelectValue>
          </SelectTrigger>
          <SelectContent>
            <SelectGroup>
              {services.map((service) => (
                <SelectItem key={service.lineageId} value={service.lineageId} label={service.name}>{service.name}</SelectItem>
              ))}
            </SelectGroup>
          </SelectContent>
        </Select>
      </Field>
    </SettingsForm>
  );
}

/** A Project's name: one DNS label, as the Store checks it; renaming it changes its URLs. */
function ProjectNameRow({ project, onRename }: { project: string; onRename: (name: string) => void }) {
  const [draft, setDraft] = useState(project);
  const name = draft.trim();
  const error = name === project ? null : dnsLabelError(name);
  return (
    <SettingsRow
      label="Name"
      hint="Changes the project URL."
      commit="confirm"
      control={(
        <ConfirmableInput id="project-name" aria-label="Name" value={draft} onValueChange={setDraft} error={error}
          isDirty={draft !== project} onCancel={() => setDraft(project)}
          onConfirm={() => { if (name !== project && !error) onRename(name); }} />
      )}
    />
  );
}

/** A Project's settings over the Config Store: its name, its Default Environment, and deleting it. */
export function StoreProjectSettings({ organizationSlug, projectSlug, environmentSlug }: Place) {
  const navigate = useNavigate();
  const writer = useStoreWriter(organizationSlug);
  const listing = useStoreView(organizationSlug, environmentsQuery(projectSlug));
  const [defaultEnvironment, holdDefault] = useCommitHold(listing.ok ? listing.value.environments.find((row) => row.default)?.name ?? null : null);
  // Deleted while open: nothing to show while the page moves on.
  if (!listing.ok) return null;
  const { environments } = listing.value;
  return (
    <div className="flex flex-col gap-6">
      <SettingsSection id="project" title="Project">
        <ProjectNameRow project={projectSlug} onRename={(name) => {
          // Awaited: the page opens the renamed Project once the Store has it; a refusal toasts and stays here.
          void writer.commit({ command: "rename_project", project: projectSlug, name }).isPersisted.promise.then(() =>
            navigate({ to: ".", params: (params) => ({ ...params, projectSlug: name }), search: (prev) => prev }), () => undefined);
        }} />
        <SettingsRow
          label="Default environment"
          hint="Opens by default."
          commit="change"
          control={(
            <Select value={defaultEnvironment} onValueChange={(next) => {
              // Shown at once; a refusal toasts and the saved one shows again.
              if (next && next !== defaultEnvironment) {
                holdDefault(next, writer.commit({ command: "set_default_environment", environment: { project: projectSlug, environment: next } }));
              }
            }}>
              <SelectTrigger id="default-environment" aria-label="Default environment">
                <SelectValue>{defaultEnvironment}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                <SelectGroup>
                  {environments.map((environment) => (
                    <SelectItem key={environment.id} value={environment.name} label={environment.name}>{environment.name}</SelectItem>
                  ))}
                </SelectGroup>
              </SelectContent>
            </Select>
          )}
        />
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
    </div>
  );
}
