import { useState } from "react";
import { PlusIcon, XIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Input } from "#/components/ui/input";
import { Field, FieldLabel, FieldDescription } from "#/components/ui/field";
import type { ServiceDrawerState } from "./useServiceDrawerState";

export function ServiceWatchPathsField({ state }: { state: ServiceDrawerState }) {
  const { service } = state;
  const [watchInput, setWatchInput] = useState("");
  function updateWatchPaths(watchPaths: string[]) {
    return state.editMetadata({ environmentId: service.environmentId, serviceId: service.id,
      edit: { kind: "policy", policy: { watchPaths } },
    });
  }
  function addWatchPath() {
    const next = watchInput.trim();
    if (!next || service.policy.watchPaths.includes(next)) return;
    // Optimistic: the metadata editor rolls back and toasts if saving fails.
    updateWatchPaths([...service.policy.watchPaths, next]);
    setWatchInput("");
  }
  return (
      <Field>
        <FieldLabel>Watch paths</FieldLabel>
        <FieldDescription>
          Gitignore-style paths that trigger a new deployment when they change.
        </FieldDescription>
        {service.policy.watchPaths.length > 0 ? (
          <div className="flex flex-wrap gap-2">
            {service.policy.watchPaths.map((path) => (
              <Badge key={path} variant="secondary">
                {path}
                <button
                  type="button"
                  aria-label={`Remove ${path}`}
                  className="ml-1 -mr-0.5 rounded-sm opacity-70 hover:opacity-100"
                  onClick={() =>
                    updateWatchPaths(
                      service.policy.watchPaths.filter((item) => item !== path)
                    )
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
            className="flex-1"
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
  );
}
