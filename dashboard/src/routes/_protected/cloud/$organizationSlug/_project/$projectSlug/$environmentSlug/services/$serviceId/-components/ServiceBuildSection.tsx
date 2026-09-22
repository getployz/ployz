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
import { ToggleGroup, ToggleGroupItem } from "#/components/ui/toggle-group";
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

export function ServiceBuildSection({
  state,
}: {
  state: ServiceDrawerState;
}) {
  const { service, collection, diff } = state;
  const builderDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.buildBuilder);
  const dockerfileDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.buildDockerfilePath);
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
    const transaction = collection.update(service.id, (draft) => {
      draft.build = { ...draft.build, ...patch };
    });
    void transaction.isPersisted.promise;
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
        <FieldLabel>Builder</FieldLabel>
        <FieldDescription>
          Build with Railpack or use your own Dockerfile.
        </FieldDescription>
        <ToggleGroup
          variant="outline"
          value={[build.builder]}
          onValueChange={(value) => {
            const [next] = value;
            if (next === "railpack" || next === "dockerfile") {
              updateBuild({ builder: next });
            }
          }}
        >
          <ToggleGroupItem data-changed={builderDiff.changed || undefined} value="railpack">Railpack</ToggleGroupItem>
          <ToggleGroupItem data-changed={builderDiff.changed || undefined} value="dockerfile">Dockerfile</ToggleGroupItem>
        </ToggleGroup>
      </Field>

      {build.builder === "dockerfile" ? (
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

    </FieldGroup>
  );
}
