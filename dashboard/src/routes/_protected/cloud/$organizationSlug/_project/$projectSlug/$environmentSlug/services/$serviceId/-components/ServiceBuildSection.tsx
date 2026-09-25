import { ServiceCommandField } from "./ServiceCommandField";
import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { githubFileSearchQueryOptions } from "#/modules/github/github.queries";
import type { PersistableTransaction } from "#/components/stageable/collection-field-resources";
import type { ServiceBuildConfig } from "#/modules/environment-design/tables";
import {
  Field,
  FieldDescription,
  FieldGroup,
  FieldLabel,
} from "#/components/ui/field";
import { Select, SelectContent, SelectGroup, SelectItem, SelectSeparator, SelectTrigger, SelectValue } from "#/components/ui/select";
import { ToggleGroup, ToggleGroupItem } from "#/components/ui/toggle-group";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { preferredBuilderSchema } from "#/modules/environment-design/service-policy";
import { Option, Schema } from "effect";
import { SERVICE_DEPLOYMENT_DIFF_PATHS } from "#/modules/services/service-deployment-diff/fields";
import { ServiceSettingInput } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceSettingInput";
import type { ServiceDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";

type GitRef = {
  repositoryId: number;
  installationId: number | null;
  ref: string;
};

function DockerfilePathInput({
  gitRef,
  value,
  isChanged,
  onCommit,
}: {
  gitRef: GitRef;
  value: string;
  isChanged: boolean;
  onCommit: (raw: string) => PersistableTransaction;
}) {
  const [search, setSearch] = useState(false);
  const files = useQuery({
    ...githubFileSearchQueryOptions({
      ...gitRef,
      pattern: "**/*Dockerfile*",
    }),
    enabled: search,
    retry: false,
  });
  const suggestions = (files.data?.paths ?? [])
    .filter((path) => !path.endsWith(".dockerignore"))
    .sort((left, right) => left.split("/").length - right.split("/").length || left.localeCompare(right));

  return (
    <ServiceSettingInput
      ariaLabel="Dockerfile path"
      placeholder="Dockerfile"
      suggestions={suggestions}
      suggestionsLoading={files.isFetching}
      suggestionsMessage={files.isError ? "Couldn’t load suggestions. Enter a path." : undefined}
      suggestionsNotice={files.data?.truncated ? "Some files are omitted. You can enter a path manually." : undefined}
      onFocus={() => setSearch(true)}
      value={value}
      isChanged={isChanged}
      onCommit={onCommit}
    />
  );
}

/** Service Deployment Policy: saved at once, never staged, so Discard doesn't undo it. */
function PreferredBuilderField({ state }: { state: ServiceDrawerState }) {
  const { service } = state;
  const { machines } = useRuntimeLens(state.organizationSlug);
  const builders = [
    { id: "github", label: "GitHub Actions" },
    ...machines.filter((machine) => machine.acceptsBuilds).map((machine) => ({ id: machine.id, label: machine.name })),
  ];
  // The collection row widens the MachineId brand; the Select speaks plain strings.
  const value = String(service.policy.preferredBuilder ?? "auto");
  const label = value === "auto" ? "Auto"
    : builders.find((builder) => builder.id === value)?.label ?? machines.find((machine) => machine.id === value)?.name ?? "A removed server";
  return (
    <Field>
      <FieldLabel>Preferred builder</FieldLabel>
      <FieldDescription>
        Tried first. If it can’t start the build in time, the build follows your organization’s build order.
      </FieldDescription>
      <Select value={value} onValueChange={(next) => {
        if (next === null || next === value) return;
        const chosen = Schema.decodeUnknownOption(preferredBuilderSchema)(next);
        if (next !== "auto" && Option.isNone(chosen)) return;
        state.editMetadata({ environmentId: service.environmentId, serviceId: service.id,
          edit: { kind: "policy", policy: { preferredBuilder: Option.getOrNull(chosen) } } });
      }}>
        <SelectTrigger aria-label="Preferred builder" className="w-64">
          <SelectValue>{label}</SelectValue>
        </SelectTrigger>
        <SelectContent>
          <SelectGroup>
            <SelectItem value="auto" label="Auto">Auto</SelectItem>
          </SelectGroup>
          <SelectSeparator />
          <SelectGroup>
            {builders.map((builder) => (
              <SelectItem key={builder.id} value={builder.id} label={builder.label}>{builder.label}</SelectItem>
            ))}
          </SelectGroup>
        </SelectContent>
      </Select>
    </Field>
  );
}

export function ServiceBuildSection({
  state,
}: {
  state: ServiceDrawerState;
}) {
  const { service, collection, diff } = state;
  const buildMethodDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.buildMethod);
  const dockerfileDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.buildDockerfilePath);
  const commandDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.buildCommand);
  const build = service.build;
  const source = service.source;
  const gitRef =
    source.type === "git" && source.branch.type === "connected"
      ? {
          repositoryId: source.repositoryId,
          installationId: source.access.type === "public" ? null : source.access.installationId,
          ref: source.branch.name,
        }
      : null;

  function updateBuild(patch: Partial<ServiceBuildConfig>) {
    collection.update(service.id, (draft) => {
      draft.build = { ...draft.build, ...patch };
    });
  }

  function commitDockerfilePath(raw: string) {
    return collection.update(service.id, (draft) => {
      draft.build = {
        ...draft.build,
        dockerfilePath: raw.length > 0 ? raw : null,
      };
    });
  }


  return (
    <FieldGroup>
      <Field>
        <FieldLabel>Build method</FieldLabel>
        <FieldDescription>
          Build with Railpack or use your own Dockerfile.
        </FieldDescription>
        <ToggleGroup
          variant="outline"
          value={[build.buildMethod]}
          onValueChange={(value) => {
            const [next] = value;
            if (next === "railpack" || next === "dockerfile") {
              updateBuild({ buildMethod: next });
            }
          }}
        >
          <ToggleGroupItem data-changed={buildMethodDiff.changed || undefined} value="railpack">Railpack</ToggleGroupItem>
          <ToggleGroupItem data-changed={buildMethodDiff.changed || undefined} value="dockerfile">Dockerfile</ToggleGroupItem>
        </ToggleGroup>
      </Field>

      {build.buildMethod === "railpack" ? (
        <ServiceCommandField
          label="Build command"
          description="Override the detected build command. Leave empty to use Railpack’s default."
          addLabel="Build command"
          placeholder="pnpm run build"
          value={build.command}
          baselineLabel={commandDiff.baselineLabel}
          baselineValue={commandDiff.baselineValue}
          isChanged={commandDiff.changed}
          onCommit={(value) => collection.update(service.id, (draft) => {
            draft.build.command = value;
          })}
        />
      ) : null}

      {build.buildMethod === "dockerfile" ? (
        <Field>
          <FieldLabel>Dockerfile path</FieldLabel>
          <FieldDescription>
            Path to the Dockerfile within the repository.
          </FieldDescription>
          {gitRef ? (
            <DockerfilePathInput
              gitRef={gitRef}
              value={build.dockerfilePath ?? ""}
              isChanged={dockerfileDiff.changed}
              onCommit={commitDockerfilePath}
            />
          ) : (
            <ServiceSettingInput
              ariaLabel="Dockerfile path"
              placeholder="Dockerfile"
              value={build.dockerfilePath ?? ""}
              isChanged={dockerfileDiff.changed}
              onCommit={commitDockerfilePath}
            />
          )}
        </Field>
      ) : null}

      <PreferredBuilderField state={state} />
    </FieldGroup>
  );
}
