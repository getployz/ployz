import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { GitBranchIcon, PencilIcon } from "lucide-react";
import type { EnvironmentRef, ServiceSettingChange } from "@ployz/sdk";
import { GitBranchSelectorDialog } from "#/components/service-source-selector";
import { Button } from "#/components/ui/button";
import { Field, FieldDescription, FieldLabel } from "#/components/ui/field";
import { Item, ItemActions, ItemContent, ItemMedia, ItemTitle } from "#/components/ui/item";
import { Select, SelectContent, SelectGroup, SelectItem, SelectSeparator, SelectTrigger, SelectValue } from "#/components/ui/select";
import { serviceSetting } from "#/modules/config-store/catalog";
import { changedProps, settingText } from "#/modules/config-store/store-services";
import { prPlansQuery } from "#/modules/config-store/store-pull-requests";
import { useCachedStoreView } from "#/modules/config-store/store-view.queries";
import { githubFileSearchQueryOptions } from "#/modules/github/github.queries";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import type { PersistableTransaction } from "#/components/stageable/collection-field-resources";
import { ServiceSettingInput } from "./ServiceSettingInput";

/**
 * A repository's GitHub ids, which branch and file pickers need: the Project's PR plans list every repository its
 * Services deploy from. Undefined until known.
 */
export function useRepositoryRef(organizationSlug: string, environment: EnvironmentRef, repository: string) {
  const plans = useCachedStoreView(organizationSlug, prPlansQuery(environment.project ?? ""));
  const plan = plans?.ok ? plans.value.plans.find((candidate) => candidate.repository === repository) : undefined;
  return plan ? { repositoryId: plan.repository_id, installationId: plan.installation_id } : undefined;
}

type GitRef = { repositoryId: number; installationId: number };

/** The branch a Git Service builds, picked from the repository's branches (typed when GitHub isn't known yet). */
export function StoreBranchField({ repository, gitRef, value, change, onSet }: {
  repository: string; gitRef: GitRef | undefined; value: string; change: ServiceSettingChange | undefined;
  onSet: (branch: string) => PersistableTransaction;
}) {
  const setting = serviceSetting("branch");
  const [picking, setPicking] = useState(false);
  return (
    <Field>
      <FieldLabel>{setting.title}</FieldLabel>
      <FieldDescription>{setting.description}</FieldDescription>
      {gitRef ? (
        <>
          <Item variant="muted" data-changed={change ? true : undefined} title={change ? `Deployed: ${settingText(change.before)}` : undefined}>
            <ItemMedia variant="icon"><GitBranchIcon /></ItemMedia>
            <ItemContent><ItemTitle>{value || "Default branch"}</ItemTitle></ItemContent>
            <ItemActions>
              <Button type="button" variant="ghost" size="icon" onClick={() => setPicking(true)}>
                <PencilIcon /><span className="sr-only">Change branch</span>
              </Button>
            </ItemActions>
          </Item>
          <GitBranchSelectorDialog open={picking} onOpenChange={setPicking} repositoryFullName={repository}
            repositoryId={gitRef.repositoryId} installationId={gitRef.installationId}
            onSelectBranch={(branch) => { void onSet(branch); setPicking(false); }} />
        </>
      ) : (
        <ServiceSettingInput ariaLabel={setting.title} placeholder="main" value={value} {...changedProps(change)} onCommit={(raw) => onSet(raw)} />
      )}
    </Field>
  );
}

/** The Dockerfile a Git Service builds, suggesting the repository's Dockerfiles once the field is focused. */
export function StoreDockerfileField({ gitRef, branch, value, change, onCommit }: {
  gitRef: GitRef | undefined; branch: string; value: string; change: ServiceSettingChange | undefined;
  onCommit: (raw: string) => PersistableTransaction;
}) {
  const setting = serviceSetting("dockerfilePath");
  const [search, setSearch] = useState(false);
  const files = useQuery({
    ...githubFileSearchQueryOptions({ repositoryId: gitRef?.repositoryId ?? 0, installationId: gitRef?.installationId ?? null,
      ref: branch, pattern: "**/*Dockerfile*" }),
    enabled: search && gitRef !== undefined && branch !== "",
    retry: false,
  });
  const suggestions = (files.data?.paths ?? []).filter((path) => !path.endsWith(".dockerignore"))
    .sort((left, right) => left.split("/").length - right.split("/").length || left.localeCompare(right));
  return (
    <Field>
      <FieldLabel>{setting.title}</FieldLabel>
      <FieldDescription>{setting.description}</FieldDescription>
      <ServiceSettingInput ariaLabel={setting.title} placeholder="Dockerfile" value={value} {...changedProps(change)}
        suggestions={gitRef ? suggestions : undefined} suggestionsLoading={files.isFetching}
        suggestionsMessage={files.isError ? "Couldn’t load suggestions. Enter a path." : undefined}
        suggestionsNotice={files.data?.truncated ? "Some files are omitted. You can enter a path manually." : undefined}
        onFocus={() => setSearch(true)} onCommit={onCommit} />
    </Field>
  );
}

/** Who builds a Git Service first; part of its Deployment Policy, so it applies at once and Discard leaves it. */
export function StorePreferredBuilderField({ organizationSlug, value, onSet }: {
  organizationSlug: string; value: string | null; onSet: (builder: string | null) => void;
}) {
  const setting = serviceSetting("preferredBuilder");
  const { machines, status } = useRuntimeLens(organizationSlug);
  const builders = [
    { id: "github", label: "GitHub Actions" },
    ...machines.filter((machine) => machine.acceptsBuilds).map((machine) => ({ id: machine.id, label: machine.name })),
  ];
  const current = value ?? "auto";
  const label = current === "auto" ? "Auto"
    : builders.find((builder) => builder.id === current)?.label
      // Only an observed machine list can say the server is gone.
      ?? (status === "observed" ? "A removed server" : "Unknown server");
  return (
    <Field>
      <FieldLabel>{setting.title}</FieldLabel>
      <FieldDescription>{setting.description}</FieldDescription>
      <Select value={current} onValueChange={(next) => { if (next !== null && next !== current) onSet(next === "auto" ? null : next); }}>
        <SelectTrigger aria-label={setting.title} className="w-64"><SelectValue>{label}</SelectValue></SelectTrigger>
        <SelectContent>
          <SelectGroup><SelectItem value="auto" label="Auto">Auto</SelectItem></SelectGroup>
          <SelectSeparator />
          <SelectGroup>
            {builders.map((builder) => <SelectItem key={builder.id} value={builder.id} label={builder.label}>{builder.label}</SelectItem>)}
          </SelectGroup>
        </SelectContent>
      </Select>
    </Field>
  );
}
