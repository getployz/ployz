import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { GitBranchIcon } from "lucide-react";
import type { EnvironmentRef, ServiceSettingChange } from "@ployz/sdk";
import { GitBranchSelectorDialog } from "#/components/service-source-selector";
import { Button } from "#/components/ui/button";
import { SettingsRow } from "#/components/settings/settings-section";
import { Select, SelectContent, SelectGroup, SelectItem, SelectSeparator, SelectTrigger, SelectValue } from "#/components/ui/select";
import { serviceSetting } from "#/modules/config-store/catalog";
import { changedProps, settingText } from "#/modules/config-store/store-services";
import { prPlansQuery } from "#/modules/config-store/store-pull-requests";
import { useCachedStoreView } from "#/modules/config-store/store-view.queries";
import { useProtectedAsk } from "#/modules/config-store/use-protected-ask";
import { githubFileSearchQueryOptions } from "#/modules/github/github.queries";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import type { PersistableTransaction } from "#/components/stageable/collection-field-resources";
import { useCommitHold } from "#/components/stageable/use-commit-hold";
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

/**
 * Why you can't connect a repository or change a deploy branch in `environment`, or undefined when you can: in a
 * Protected Environment only those who may protect it do. Undefined until the listing and your role load; the Store refuses either way.
 */
export function useSourceLock(organizationSlug: string, environment: EnvironmentRef) {
  return useProtectedAsk(organizationSlug, environment) ? "Only an owner or admin can change this." : undefined;
}

/** The branch a Git Service builds, picked from the repository's branches (typed when GitHub isn't known yet). */
export function StoreBranchField({ organizationSlug, repository, gitRef, value, change, locked, onSet }: {
  organizationSlug: string; repository: string; gitRef: GitRef | undefined; value: string; change: ServiceSettingChange | undefined;
  /** Why the branch can't change here, when it can't. */
  locked?: string;
  onSet: (branch: string) => PersistableTransaction;
}) {
  const setting = serviceSetting("branch");
  const [picking, setPicking] = useState(false);
  const [shown, hold] = useCommitHold(value);
  if (locked) {
    return (
      <SettingsRow label={setting.title} hint={locked} commit="none" control={
        <Button type="button" variant="outline" disabled data-changed={change ? true : undefined}>
          <GitBranchIcon data-icon="inline-start" />{shown || "Default branch"}<span className="sr-only">Change branch</span>
        </Button>
      } />
    );
  }
  if (!gitRef) {
    return (
      <SettingsRow label={setting.title} hint={setting.description} commit="confirm"
        control={<ServiceSettingInput ariaLabel={setting.title} placeholder="main" value={value} {...changedProps(change)} onCommit={(raw) => onSet(raw)} />} />
    );
  }
  return (
    <>
      <SettingsRow label={setting.title} hint={setting.description} commit="none" control={
        <Button type="button" variant="outline" data-changed={change ? true : undefined}
          title={change ? `Deployed: ${settingText(change.before)}` : undefined} onClick={() => setPicking(true)}>
          <GitBranchIcon data-icon="inline-start" />{shown || "Default branch"}<span className="sr-only">Change branch</span>
        </Button>
      } />
      <GitBranchSelectorDialog open={picking} onOpenChange={setPicking} organizationSlug={organizationSlug} repositoryFullName={repository}
        repositoryId={gitRef.repositoryId} installationId={gitRef.installationId}
        onSelectBranch={(branch) => { hold(branch, onSet(branch)); setPicking(false); }} />
    </>
  );
}

/** The Dockerfile a Git Service builds, suggesting the repository's Dockerfiles once the field is focused. */
export function StoreDockerfileField({ organizationSlug, gitRef, branch, value, change, onCommit }: {
  organizationSlug: string; gitRef: GitRef | undefined; branch: string; value: string; change: ServiceSettingChange | undefined;
  onCommit: (raw: string) => PersistableTransaction;
}) {
  const setting = serviceSetting("dockerfilePath");
  const [search, setSearch] = useState(false);
  const files = useQuery({
    ...githubFileSearchQueryOptions({ organizationSlug, repositoryId: gitRef?.repositoryId ?? 0, installationId: gitRef?.installationId ?? null,
      ref: branch, pattern: "**/*Dockerfile*" }),
    enabled: search && gitRef !== undefined && branch !== "",
    retry: false,
  });
  const suggestions = (files.data?.paths ?? []).filter((path) => !path.endsWith(".dockerignore"))
    .sort((left, right) => left.split("/").length - right.split("/").length || left.localeCompare(right));
  return (
    <SettingsRow label={setting.title} hint={setting.description} commit="confirm" control={
      <ServiceSettingInput ariaLabel={setting.title} placeholder="Dockerfile" value={value} {...changedProps(change)}
        suggestions={gitRef ? suggestions : undefined} suggestionsLoading={files.isFetching}
        suggestionsMessage={files.isError ? "Couldn’t load suggestions. Enter a path." : undefined}
        suggestionsNotice={files.data?.truncated ? "Some files are omitted. You can enter a path manually." : undefined}
        onFocus={() => setSearch(true)} onCommit={onCommit} />
    } />
  );
}

/** Who builds a Git Service first; part of its Deployment Policy, so it applies at once and Discard leaves it. */
export function StorePreferredBuilderField({ organizationSlug, value, sealed, onSet }: {
  organizationSlug: string; value: string | null;
  /** The first of the Service's variables the browser sealed: GitHub never builds it. */
  sealed: string | null;
  onSet: (builder: string | null) => PersistableTransaction;
}) {
  const setting = serviceSetting("preferredBuilder");
  const { machines, status } = useRuntimeLens(organizationSlug);
  const builders = [
    { id: "github", label: "GitHub Actions", refusal: sealed === null ? null : `Uses sealed ${sealed}` },
    ...machines.filter((machine) => machine.acceptsBuilds).map((machine) => ({ id: machine.id, label: machine.name, refusal: null })),
  ];
  const [current, hold] = useCommitHold(value ?? "auto");
  const label = current === "auto" ? "Auto"
    : builders.find((builder) => builder.id === current)?.label
      // Only an observed machine list can say the server is gone.
      ?? (status === "observed" ? "A removed server" : "Unknown server");
  return (
    <SettingsRow label={setting.title} hint={setting.description} commit="change" control={
      <Select value={current} onValueChange={(next) => { if (next !== null && next !== current) hold(next, onSet(next === "auto" ? null : next)); }}>
        <SelectTrigger aria-label={setting.title}><SelectValue>{label}</SelectValue></SelectTrigger>
        <SelectContent>
          <SelectGroup><SelectItem value="auto" label="Auto">Auto</SelectItem></SelectGroup>
          <SelectSeparator />
          <SelectGroup>
            {builders.map((builder) => (
              <SelectItem key={builder.id} value={builder.id} label={builder.label} disabled={builder.refusal !== null}>
                <span className="flex flex-col">
                  <span>{builder.label}</span>
                  {builder.refusal === null ? null : <span className="text-xs text-muted-foreground">{builder.refusal}</span>}
                </span>
              </SelectItem>
            ))}
          </SelectGroup>
        </SelectContent>
      </Select>
    } />
  );
}
