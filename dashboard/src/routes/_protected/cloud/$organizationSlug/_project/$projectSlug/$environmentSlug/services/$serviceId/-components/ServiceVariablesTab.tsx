import { Suspense, useState, type ReactNode } from "react";
import type { EnvironmentRef, EnvironmentView, ServiceListing } from "@ployz/sdk";
import { serviceSettingRows } from "#/modules/config-store/store-services";
import { serviceVariables, storeManagedExports, storeReferenceTargets, storeVariableWriter } from "#/modules/config-store/store-variables";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { domainsQuery, requireView, useStoreView } from "#/modules/config-store/store-view.queries";
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
  const variables = serviceVariables(serviceSettingRows(settings, service.name), service.id, changes);
  const writer = storeVariableWriter(store, environment, service.name, variables);

  return (
    <ServiceVariablesView
      variables={variables}
      writer={writer}
      // Its public domain needs the domains view, which looks at the Cluster: the rest shows without waiting for it.
      managed={(
        <Suspense fallback={<ManagedVariables managed={storeManagedExports(service, settings.environment, [])} />}>
          <StoreManagedVariables organizationSlug={organizationSlug} environment={environment} service={service} settings={settings} />
        </Suspense>
      )}
      valueTargets={storeReferenceTargets(settings, services, service.id)}
      allowSealOnCreate
      onCreateVariable={({ key, value, sealed, exported }) => writer.create(key, value, sealed, exported)}
      onSealVariable={(variable) => writer.seal(variable.key, variable.value.value)}
      onUpdateMetadata={(variable, patch) => writer.export(variable.key, patch.exported ?? variable.exported)}
      onApplyRaw={({ creates, updates, deletes }) => writer.replace([...creates, ...updates], deletes)}
    />
  );
}

/** A Service's variables tab: its variables, the raw editor and the variables Ployz adds. */
function ServiceVariablesView({
  variables, writer, managed, valueTargets, onCreateVariable, onSealVariable, onUpdateMetadata, onApplyRaw, allowSealOnCreate = false,
}: {
  variables: VariableRecord[];
  writer: VariableWriter;
  /** The variables Ployz adds. */
  managed: ReactNode;
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

/** The variables Ployz adds, with the public domain once the Service's domains are read. */
function StoreManagedVariables({ organizationSlug, environment, service, settings }: {
  organizationSlug: string; environment: EnvironmentRef; service: ServiceListing; settings: EnvironmentView;
}) {
  const domains = requireView(useStoreView(organizationSlug, domainsQuery(environment))).domains
    .filter((domain) => domain.service === service.name);
  return <ManagedVariables managed={storeManagedExports(service, settings.environment, domains)} />;
}

/** None of them is a secret, so each shows its value. */
function ManagedVariables({ managed }: { managed: readonly { key: string; value: string }[] }) {
  return (
    <section>
      <h2 className="font-medium">{managed.length} Ployz variables</h2>
      <div className="pt-2">
        <p className="text-sm text-muted-foreground">Ployz adds these system variables to every build and deploy.</p>
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
