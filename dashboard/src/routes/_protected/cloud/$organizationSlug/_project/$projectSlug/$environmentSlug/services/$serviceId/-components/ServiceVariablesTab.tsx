import { useRuntimeStatus } from "#/providers/runtime-provider";
import { useEnvironmentDocumentEditor } from "#/modules/environment-design/environment-document-edit";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { variableDocumentRecord } from "#/modules/environment-design/variable-document";
import { useState } from "react";
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
import { useSealServiceVariableAction } from "#/modules/environment-design/variable-mutation-actions";
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
  const { hostedDnsHostname } = useRuntimeStatus();
  const ployzManagedVariables = getManagedServiceExports(state.service, hostedDnsHostname);
  const variableWriter = useVariableWriter(state.organizationSlug);
  const updateExport = useServerFn(updateServiceVariableExportServerFn);
  const [rawEditorOpen, setRawEditorOpen] = useState(false);

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
    <TabsContent value="variables" className="mt-4 overflow-y-auto"><div className="mx-auto w-full max-w-2xl">
      <VariablesPanel
        variables={variables}
        collection={variableWriter}
        countNoun="Service Variable"
        onCreateVariable={handleCreateVariable}
        onSealVariable={sealVariable}
        onUpdateMetadata={handleUpdateMetadata}
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
                {ployzManagedVariables.length} Ployz variables
              </h2>
                <div className="pt-2">
                  <p className="text-sm text-muted-foreground">
                    Ployz adds these system variables to every build and deploy.
                  </p>
                  <div className="mt-4">
                      {ployzManagedVariables.map((variable) => (
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
        organizationSlug={state.organizationSlug}
        environmentId={state.service.environmentId}
        serviceId={state.service.id}
        variables={variables}
        valueTargets={valueTargets}
      />
    </div></TabsContent>
  );
}
