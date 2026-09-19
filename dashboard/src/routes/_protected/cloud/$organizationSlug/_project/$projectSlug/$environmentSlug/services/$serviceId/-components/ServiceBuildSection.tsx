import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { githubFileSearchQueryOptions } from "#/modules/github/github.queries";
import type { PersistableTransaction } from "#/components/stageable/collection-field-resources";
import { PlusIcon, XIcon } from "lucide-react";
import type { ServiceBuildConfig } from "#/modules/environment-design/tables";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import {
  Field,
  FieldDescription,
  FieldGroup,
  FieldLabel,
} from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { ToggleGroup, ToggleGroupItem } from "#/components/ui/toggle-group";
import { SERVICE_DEPLOYMENT_DIFF_PATHS } from "#/modules/services/service-deployment-diff/fields";
import { ServiceSettingInput } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceSettingInput";
import type { ServiceDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";

type GitRef = {
  repositoryId: number;
  installationId: number;
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
  const buildDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.build);
  const build = service.build;
  const [watchInput, setWatchInput] = useState("");
  const source = service.source;
  const gitRef =
    source.type === "git" && source.branch.type === "connected"
      ? {
          repositoryId: source.repositoryId,
          installationId: source.installationId,
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

  function addWatchPath() {
    const next = watchInput.trim();
    if (next.length === 0 || build.watchPaths.includes(next)) {
      return;
    }
    updateBuild({ watchPaths: [...build.watchPaths, next] });
    setWatchInput("");
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
          <ToggleGroupItem value="railpack">Railpack</ToggleGroupItem>
          <ToggleGroupItem value="dockerfile">Dockerfile</ToggleGroupItem>
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
              isChanged={buildDiff.changed}
              onCommit={commitDockerfilePath}
            />
          ) : (
            <ServiceSettingInput
              ariaLabel="Dockerfile path"
              placeholder="Dockerfile"
              value={build.dockerfilePath ?? ""}
              isChanged={buildDiff.changed}
              onCommit={commitDockerfilePath}
            />
          )}
        </Field>
      ) : null}

      <Field>
        <FieldLabel>Watch paths</FieldLabel>
        <FieldDescription>
          Gitignore-style paths that trigger a new deployment when they change.
        </FieldDescription>
        {build.watchPaths.length > 0 ? (
          <div className="flex flex-wrap gap-2">
            {build.watchPaths.map((path) => (
              <Badge key={path} variant="secondary" className="font-mono">
                {path}
                <button
                  type="button"
                  aria-label={`Remove ${path}`}
                  className="ml-1 -mr-0.5 rounded-sm opacity-70 hover:opacity-100"
                  onClick={() =>
                    updateBuild({
                      watchPaths: build.watchPaths.filter((item) => item !== path),
                    })
                  }
                >
                  <XIcon className="size-3" />
                </button>
              </Badge>
            ))}
          </div>
        ) : null}
        <div className="flex items-center gap-2">
          <Input
            aria-label="New watch path"
            placeholder="/src/**"
            className="flex-1 font-mono"
            value={watchInput}
            onChange={(event) => setWatchInput(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                addWatchPath();
              }
            }}
          />
          <Button type="button" variant="outline" onClick={addWatchPath}>
            <PlusIcon data-icon="inline-start" />
            Add pattern
          </Button>
        </div>
      </Field>
    </FieldGroup>
  );
}
