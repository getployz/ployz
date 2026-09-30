import { useState } from "react";
import type { Change, EnvironmentRef, EnvironmentView, JsonValue, ServiceListing } from "@ployz/sdk";
import { serviceSettingRows } from "#/modules/config-store/store-services";
import { serviceVariables, storeManagedExports, storeReferenceTargets, storeVariableWriter } from "#/modules/config-store/store-variables";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { BracesIcon } from "lucide-react";
import { SecretValueDisplay } from "#/components/secret-value-display";
import { Button } from "#/components/ui/button";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyTitle,
} from "#/components/ui/empty";
import { Separator } from "#/components/ui/separator";
import { TabsContent } from "#/components/ui/tabs";
import {
  VariablesPanel,
  type VariableAddInput,
} from "#/components/variables/variables-panel";
import type { VariableMetadataPatch } from "#/components/variables/variable-row";
import type { PlainVariableRecord, VariableRecord, VariableWriter } from "#/modules/variables/variables";
import type { ReferenceTarget } from "#/modules/variables/variable-autocomplete";
import type { RawEditorDiff } from "#/modules/variables/variable-raw-editor";
import { ServiceVariablesRawEditor } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceVariablesRawEditor";

/** A Config Store Service's variables: each edit is one optimistic write of `SERVICE.env.KEY`; a secret never reads back. */
export function StoreServiceVariablesTab({ organizationSlug, environment, service, services, settings, changes }: {
  organizationSlug: string;
  environment: EnvironmentRef;
  service: ServiceListing;
  services: readonly ServiceListing[];
  /** The Environment's Settings, with this tab's pending edits over them. */
  settings: EnvironmentView;
  /** What the next Deploy changes in it, by Setting. */
  changes: ReadonlyMap<string, unknown>;
}) {
  const store = useStoreWriter(organizationSlug);
  const variables = serviceVariables(serviceSettingRows(settings, service.name), service.id, changes);
  const writer = storeVariableWriter(store, environment, service.name, variables);
  const set = (key: string, value: JsonValue) =>
    store.edit({ environment, changes: [{ op: "set", path: `${service.name}.env.${key}`, value }] });

  return (
    <ServiceVariablesView
      variables={variables}
      writer={writer}
      managed={storeManagedExports(service, settings.environment)}
      valueTargets={storeReferenceTargets(settings, services, service.id)}
      allowSealOnCreate
      onCreateVariable={({ key, value, sealed, exported }) => store.edit({ environment, changes: [
        { op: "set", path: `${service.name}.env.${key}`, value: sealed ? { secret: value } : value },
        { op: "set", path: `${service.name}.env.${key}.exported`, value: exported },
      ] })}
      onSealVariable={(variable) => set(variable.key, { secret: variable.value.value })}
      onUpdateMetadata={(variable, patch) => set(`${variable.key}.exported`, patch.exported ?? variable.exported)}
      onApplyRaw={({ creates, updates, deletes }) => store.edit({
        environment,
        changes: [
          ...[...creates, ...updates].map(({ key, value }): Change => ({ op: "set", path: `${service.name}.env.${key}`, value })),
          ...deletes.map((key): Change => ({ op: "unset", path: `${service.name}.env.${key}` })),
        ],
      })}
    />
  );
}

/** A Service's variables tab: its variables, the raw editor and the variables Ployz adds. */
function ServiceVariablesView({
  variables, writer, managed, valueTargets, onCreateVariable, onSealVariable, onUpdateMetadata, onApplyRaw, allowSealOnCreate = false,
}: {
  variables: VariableRecord[];
  writer: VariableWriter;
  managed: readonly { key: string; value: string }[];
  valueTargets: ReferenceTarget[];
  onCreateVariable: (input: VariableAddInput) => void;
  onSealVariable: (variable: PlainVariableRecord) => void;
  onUpdateMetadata: (variable: VariableRecord, patch: VariableMetadataPatch) => void;
  onApplyRaw: (diff: RawEditorDiff) => void;
  allowSealOnCreate?: boolean;
}) {
  const [rawEditorOpen, setRawEditorOpen] = useState(false);
  return (
    <TabsContent value="variables" className="mt-4 overflow-y-auto"><div className="mx-auto w-full max-w-2xl">
      <VariablesPanel
        variables={variables}
        collection={writer}
        allowSealOnCreate={allowSealOnCreate}
        countNoun="Service Variable"
        onCreateVariable={onCreateVariable}
        onSealVariable={onSealVariable}
        onUpdateMetadata={onUpdateMetadata}
        valueTargets={valueTargets}
        headerActions={
          <Button
            type="button"
            variant="ghost"
            onClick={() => setRawEditorOpen(true)}
          >
            <BracesIcon data-icon="inline-start" />
            Raw editor
          </Button>
        }
        renderAfterList={() => (
          <>
            <Separator />
            <section>
              <h2 className="font-medium">
                {managed.length} Ployz variables
              </h2>
                <div className="pt-2">
                  <p className="text-sm text-muted-foreground">
                    Ployz adds these system variables to every build and deploy.
                  </p>
                  <div className="mt-4">
                      {managed.map((variable) => (
                        <div key={variable.key} className="grid grid-cols-2 items-center gap-3 border-b py-2 last:border-b-0">
                          <div className="min-w-0 truncate font-mono text-sm" title={variable.key}>
                            {variable.key}
                          </div>
                          <div className="flex min-w-0 items-center gap-1.5">
                            <SecretValueDisplay
                              value={variable.value}
                            />
                            <span className="size-7 shrink-0" aria-hidden="true" />
                          </div>
                        </div>
                      ))}
                  </div>
                </div>
            </section>
          </>
        )}
        emptyState={
          <Empty>
            <EmptyHeader>
              <EmptyTitle>No variables yet</EmptyTitle>
              <EmptyDescription>
                Add variables one by one or paste them into the{" "}
                <button
                  type="button"
                  className="underline underline-offset-4"
                  onClick={() => setRawEditorOpen(true)}
                >
                  raw editor
                </button>
              </EmptyDescription>
            </EmptyHeader>
          </Empty>
        }
      />

      <ServiceVariablesRawEditor
        open={rawEditorOpen}
        onOpenChange={setRawEditorOpen}
        onApply={onApplyRaw}
        variables={variables}
        valueTargets={valueTargets}
      />
    </div></TabsContent>
  );
}
