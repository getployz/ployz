import { useState } from "react";
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

export function ServiceBuildSection({
  state,
}: {
  state: ServiceDrawerState;
}) {
  const { service, collection, diff } = state;
  const buildDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.build);
  const build = service.build;
  const [watchInput, setWatchInput] = useState("");

  function updateBuild(patch: Partial<ServiceBuildConfig>) {
    const transaction = collection.update(service.id, (draft) => {
      draft.build = { ...draft.build, ...patch };
    });
    void transaction.isPersisted.promise;
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
          Build from a Dockerfile, or let the platform detect a builder.
        </FieldDescription>
        <ToggleGroup
          variant="outline"
          value={[build.builder]}
          onValueChange={(value) => {
            const [next] = value;
            if (next === "auto" || next === "dockerfile") {
              updateBuild({ builder: next });
            }
          }}
        >
          <ToggleGroupItem value="auto">Auto-detect</ToggleGroupItem>
          <ToggleGroupItem value="dockerfile">Dockerfile</ToggleGroupItem>
        </ToggleGroup>
      </Field>

      {build.builder === "dockerfile" ? (
        <Field>
          <FieldLabel>Dockerfile path</FieldLabel>
          <FieldDescription>
            Path to the Dockerfile within the repository.
          </FieldDescription>
          <ServiceSettingInput
            ariaLabel="Dockerfile path"
            placeholder="Dockerfile"
            value={build.dockerfilePath ?? ""}
            isChanged={buildDiff.changed}
            onCommit={(raw) =>
              collection.update(service.id, (draft) => {
                draft.build = {
                  ...draft.build,
                  dockerfilePath: raw.length > 0 ? raw : null,
                };
              })
            }
          />
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
