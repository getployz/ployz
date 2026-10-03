import { useState, type ReactNode } from "react";
import type { Persistable } from "#/collections/query-collection";
import type { EnvironmentRef, EnvironmentView, ServiceListing } from "@ployz/sdk";
import { serviceSettingRows } from "#/modules/config-store/store-services";
import { serviceVariables, storeManagedExports, storeReferenceTargets, storeVariableWriter } from "#/modules/config-store/store-variables";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { BracesIcon } from "lucide-react";
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
  const rows = serviceSettingRows(settings, service.name);
  const variables = serviceVariables(rows, service.id, changes, new Set(settings.never_synced));
  const writer = storeVariableWriter(store, environment, service.name, variables, rows);

  return (
    <ServiceVariablesView
      variables={variables}
      writer={writer}
      managed={<ManagedVariables managed={storeManagedExports(service, settings.environment)} />}
      valueTargets={storeReferenceTargets(settings, services, service.id)}
      serviceNames={services.map((listed) => listed.name)}
      allowSealOnCreate
      onCreateVariable={({ key, value, sealed, exported }) => writer.create(key, value, sealed, exported)}
      onSealVariable={(variable) => writer.seal(variable.key, variable.value.value)}
      onUpdateMetadata={(variable, patch) => writer.export(variable.key, patch.exported ?? variable.exported)}
      onApplyRaw={({ creates, updates, deletes }) => writer.replace([...creates, ...updates], deletes)}
      onNeverSync={(variable, marked) => writer.neverSync(variable.key, marked)}
    />
  );
}

/** A Service's variables tab: its variables, the raw editor and the variables Ployz adds. */
function ServiceVariablesView({
  variables, writer, managed, valueTargets, serviceNames, onCreateVariable, onSealVariable, onUpdateMetadata, onApplyRaw, onNeverSync, allowSealOnCreate = false,
}: {
  variables: VariableRecord[];
  writer: VariableWriter;
  /** The variables Ployz adds. */
  managed: ReactNode;
  valueTargets: ReferenceTarget[];
  serviceNames: readonly string[];
  onCreateVariable: (input: VariableAddInput) => Persistable;
  onSealVariable: (variable: PlainVariableRecord) => void;
  onUpdateMetadata: (variable: VariableRecord, patch: VariableMetadataPatch) => void;
  onApplyRaw: (diff: RawEditorDiff) => Persistable;
  onNeverSync: (variable: VariableRecord, marked: boolean) => void;
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
        onNeverSync={onNeverSync}
        valueTargets={valueTargets}
        serviceNames={serviceNames}
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
        renderAfterList={() => <><Separator />{managed}</>}
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

/** None of them is a secret, so each shows its value. */
function ManagedVariables({ managed }: { managed: readonly { key: string; value: string }[] }) {
  return (
    <section>
      <h2 className="font-medium">{managed.length} Ployz reference defaults</h2>
      <div className="pt-2">
        <p className="text-sm text-muted-foreground">Available in {"${{ }}"} references. Variables you set take precedence.</p>
        <div className="mt-4">
          {managed.map((variable) => (
            <div key={variable.key} className="grid grid-cols-2 items-center gap-3 border-b py-2 last:border-b-0">
              <div className="min-w-0 truncate font-mono text-sm" title={variable.key}>{variable.key}</div>
              <div className="min-w-0 truncate font-mono text-sm text-muted-foreground" title={variable.value}>{variable.value}</div>
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}
