import { useClusterDomainName } from "#/modules/cluster-domain/use-cluster-domain";
import { useEnvironmentDocumentEditor } from "#/modules/environment-design/environment-document-edit";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { variableDocumentRecord } from "#/modules/environment-design/variable-document";
import { useState } from "react";
import type { Change, EnvironmentRef, EnvironmentView, JsonValue, ServiceListing } from "@ployz/sdk";
import { serviceSettingRows } from "#/modules/config-store/store-services";
import { serviceVariables, storeManagedExports, storeReferenceTargets, storeVariableWriter } from "#/modules/config-store/store-variables";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { useServerFn } from "@tanstack/react-start";
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
import { useReferenceTargets } from "#/components/variables/use-reference-targets";
import type { VariableRecord } from "#/modules/environment-design/variables";
import { getManagedServiceExports } from "#/modules/environment-design/managed-service-exports";
import { useApplyRawVariablesAction, useSealServiceVariableAction, type PlainVariableRecord } from "#/modules/environment-design/variable-mutation-actions";
import type { VariableWriter } from "#/modules/environment-design/variable-collections";
import type { ReferenceTarget } from "#/modules/environment-design/variable-autocomplete";
import type { RawEditorDiff } from "#/modules/environment-design/variable-raw-editor";
import { updateServiceVariableExportServerFn } from "#/modules/environment-design/variable-functions";
import { insertPlainServiceVariable } from "#/modules/environment-design/variable-collections";
import { useVariableWriter } from "#/modules/services/services.collection";
import { ServiceVariablesRawEditor } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceVariablesRawEditor";
import type { ServiceDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";

export function ServiceVariablesTab({
  state,
}: {
  state: ServiceDrawerState;
}) {
  const editDocument = useEnvironmentDocumentEditor(state.organizationSlug);
  const clusterDomain = useClusterDomainName(state.organizationSlug);
  const ployzManagedVariables = getManagedServiceExports(state.service, clusterDomain);
  const variableWriter = useVariableWriter(state.organizationSlug);
  const updateExport = useServerFn(updateServiceVariableExportServerFn);

  const document = useEnvironmentDocument(state.organizationSlug, state.service.environmentId);
  const node = document?.intent.services.find((node) => node.id === state.service.id);
  const variables = document && node ? node.variables.map((variable) => variableDocumentRecord(variable,
    node.id, document.intent, document.updatedAt)).sort((a, b) => a.key.localeCompare(b.key)) : [];

  const valueTargets = useReferenceTargets({
    organizationSlug: state.organizationSlug,
    environmentId: state.service.environmentId,
    serviceId: state.service.id,
  });

  const sealVariable = useSealServiceVariableAction({
    organizationSlug: state.organizationSlug,
    environmentId: state.service.environmentId,
    serviceId: state.service.id,
  });

  const applyRawVariables = useApplyRawVariablesAction({
    organizationSlug: state.organizationSlug,
    environmentId: state.service.environmentId,
    serviceId: state.service.id,
  });

  function handleCreateVariable(input: VariableAddInput) {
    // Optimistic: the writer rolls back and toasts if saving fails.
    insertPlainServiceVariable(variableWriter, {
      serviceId: state.service.id,
      key: input.key,
      value: input.value,
      exported: input.exported,
    });
  }

  function handleUpdateMetadata(variable: VariableRecord, patch: VariableMetadataPatch) {
    const { organizationSlug } = state;
    const { environmentId, id: serviceId } = state.service;
    const exported = patch.exported ?? variable.exported;
    editDocument({
      environmentId,
      apply: (intent) => {
        const entry = intent.services.find((node) => node.id === serviceId)?.variables.find((entry) => entry.id === variable.id);
        if (entry) entry.exported = exported;
      },
      save: (revision) => updateExport({ data: { organizationSlug, revision, environmentId, serviceId, variableId: variable.id, exported } }),
      failureMessage: "Could not update this variable.",
    });
  }

  return (
    <ServiceVariablesView
      variables={variables}
      writer={variableWriter}
      managed={ployzManagedVariables}
      valueTargets={valueTargets}
      onCreateVariable={handleCreateVariable}
      onSealVariable={sealVariable}
      onUpdateMetadata={handleUpdateMetadata}
      onApplyRaw={applyRawVariables}
    />
  );
}

/** A Config Store Service's variables: each edit is one optimistic write of `SERVICE.env.KEY`; a secret never reads back. */
export function StoreServiceVariablesTab({ organizationSlug, environment, service, services, settings }: {
  organizationSlug: string;
  environment: EnvironmentRef;
  service: ServiceListing;
  services: readonly ServiceListing[];
  /** The Environment's Settings, with this tab's pending edits over them. */
  settings: EnvironmentView;
}) {
  const store = useStoreWriter(organizationSlug);
  const variables = serviceVariables(serviceSettingRows(settings, service.name), service.id);
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
      onCreateVariable={({ key, value, sealed, exported }) => set(key, { value: sealed ? { secret: value } : value, exported })}
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
