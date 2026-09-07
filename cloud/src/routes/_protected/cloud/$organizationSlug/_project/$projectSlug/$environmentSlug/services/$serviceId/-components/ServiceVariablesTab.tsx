import { useState } from "react";
import { eq, useLiveSuspenseQuery } from "@tanstack/react-db";
import { useServerFn } from "@tanstack/react-start";
import {
  BracesIcon,
  ChevronRightIcon,
  DatabaseZapIcon,
  Link2Icon,
  InfoIcon,
  XIcon,
} from "lucide-react";
import { Alert, AlertAction, AlertDescription } from "#/components/ui/alert";
import { SecretValueDisplay } from "#/components/secret-value-display";
import { Button } from "#/components/ui/button";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "#/components/ui/collapsible";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyTitle,
} from "#/components/ui/empty";
import { Separator } from "#/components/ui/separator";
import {
  Table,
  TableBody,
  TableCell,
  TableRow,
} from "#/components/ui/table";
import { TabsContent } from "#/components/ui/tabs";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "#/components/ui/tooltip";
import {
  VariablesPanel,
  type VariableAddInput,
} from "#/components/variables/variables-panel";
import type { VariableMetadataPatch } from "#/components/variables/variable-row";
import { useReferenceTargets } from "#/components/variables/use-reference-targets";
import { getRawVariablesCollection } from "#/electric/collections";
import { parseLiveQueryRow } from "#/lib/tanstack-db";
import { decodeStrict } from "#/modules/environment-design/schema";
import { variableGroupResourceRecordSchema } from "#/modules/environment-design/resources";
import {
  environmentServiceVariableGroupAttachmentSchema,
  variableSelectSchema,
  variableValueSchema,
  type EnvironmentServiceVariableGroupAttachment,
  type VariableRecord,
} from "#/modules/environment-design/variables";
import { getManagedServiceExports } from "#/modules/environment-design/managed-service-exports";
import { useSealServiceVariableAction } from "#/modules/environment-design/variable-mutation-actions";
import { updateServiceVariableExportServerFn } from "#/modules/environment-design/variable-functions";
import { insertPlainServiceVariable } from "#/modules/environment-design/variable-collections";
import {
  useEnvironmentResourcesCollection,
  useServiceVariableGroupAttachmentsCollection,
  useVariableWriter,
  useVariablesCollection,
} from "#/modules/services/services.collection";
import { ServiceVariableGroupAttachmentsPanel } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceVariableGroupAttachmentsPanel";
import { ServiceVariablesRawEditor } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceVariablesRawEditor";
import type { ServiceDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";

export function ServiceVariablesTab({
  state,
}: {
  state: ServiceDrawerState;
}) {
  const ployzManagedVariables = getManagedServiceExports(state.service);
  const environmentResourcesCollection = useEnvironmentResourcesCollection(
    state.organizationSlug,
  );
  const serviceVariableGroupAttachments =
    useServiceVariableGroupAttachmentsCollection(state.organizationSlug);
  const variablesCollection = useVariablesCollection(state.organizationSlug);
  const variableWriter = useVariableWriter(state.organizationSlug);
  const updateExport = useServerFn(updateServiceVariableExportServerFn);
  const [rawEditorOpen, setRawEditorOpen] = useState(false);

  const { data: rawVariables } = useLiveSuspenseQuery({
    query: (q) =>
      q
        .from({ variable: variablesCollection })
        .where(({ variable }) => eq(variable.serviceId, state.service.id))
        .orderBy(({ variable }) => variable.key)
        .select(({ variable }) => variable),
  });
  const variables = rawVariables.map((row) =>
    parseLiveQueryRow(variableSelectSchema, row),
  );
  const { data: rawAttachmentRows } =
    useLiveSuspenseQuery({
      query: (q) =>
        q
          .from({ attachment: serviceVariableGroupAttachments })
          .where(({ attachment }) => eq(attachment["serviceId"], state.service.id))
          .select(({ attachment }) => attachment),
    });
  const { data: environmentResourceRows } =
    useLiveSuspenseQuery({
      query: (q) =>
        q
          .from({ resource: environmentResourcesCollection })
          .where(({ resource }) =>
            eq(resource.resource.environmentId, state.service.environmentId),
          )
          .select(({ resource }) => resource),
    });
  const attachments = rawAttachmentRows.map((row) =>
    parseLiveQueryRow(environmentServiceVariableGroupAttachmentSchema, row),
  );
  const environmentResources = environmentResourceRows.map((row) =>
    parseLiveQueryRow(variableGroupResourceRecordSchema, row),
  );
  const attachmentBySetId = new Map(
    attachments.map((attachment) => [attachment.variableGroupId, attachment]),
  );
  const inheritedVariables = environmentResources
    .flatMap((resource) => {
      const attachment = attachmentBySetId.get(resource.variableGroup.id);
      if (!attachment) return [];
      return resource.variables.flatMap((variable) =>
        variable.exported
          ? [{
              attachment,
              resourceName: resource.resource.name,
              variable: {
                ...variable,
                value: decodeStrict(variableValueSchema, variable.value),
              },
            }]
          : [],
      );
    })
    .sort((left, right) => {
      const orderDelta = left.attachment.sortOrder - right.attachment.sortOrder;
      if (orderDelta !== 0) return orderDelta;
      return left.variable.key.localeCompare(right.variable.key);
    });
  const inheritedKeyCounts = inheritedVariables.reduce(
    (counts, item) =>
      counts.set(item.variable.key, (counts.get(item.variable.key) ?? 0) + 1),
    new Map<string, number>(),
  );
  const inheritedKeys = new Set(inheritedVariables.map((item) => item.variable.key));
  const serviceVariableWarnings = new Map(
    variables.flatMap((variable) =>
      inheritedKeys.has(variable.key)
        ? [
            [
              variable.id,
              "Overridden by an attached Variable Group variable.",
            ] as const,
          ]
        : [],
    ),
  );

  const valueTargets = useReferenceTargets({
    organizationSlug: state.organizationSlug,
    environmentId: state.service.environmentId,
    owner: { kind: "service", serviceId: state.service.id },
  });

  const sealVariable = useSealServiceVariableAction({
    organizationSlug: state.organizationSlug,
    environmentId: state.service.environmentId,
    serviceId: state.service.id,
  });

  async function handleCreateVariable(input: VariableAddInput) {
    await insertPlainServiceVariable(variableWriter, {
      serviceId: state.service.id,
      key: input.key,
      value: input.value,
      exported: input.exported,
    });
  }

  async function handleUpdateMetadata(
    variable: VariableRecord,
    patch: VariableMetadataPatch,
  ) {
    const receipt = await updateExport({
      data: {
        organizationSlug: state.organizationSlug,
        environmentId: state.service.environmentId,
        serviceId: state.service.id,
        variableId: variable.id,
        exported: patch.exported ?? variable.exported,
      },
    });
    await getRawVariablesCollection(state.organizationSlug).utils.awaitTxId(
      receipt.txid,
    );
  }

  return (
    <TabsContent value="variables" className="mt-4 overflow-y-auto">
      <VariablesPanel
        variables={variables}
        collection={variableWriter}
        countNoun="Service Variable"
        onCreateVariable={handleCreateVariable}
        onSealVariable={sealVariable}
        onUpdateMetadata={handleUpdateMetadata}
        variableWarnings={serviceVariableWarnings}
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
        renderBeforeList={() => (
          <>
            <Alert>
              <DatabaseZapIcon />
              <AlertDescription>
                Connecting a database?{" "}
                <button
                  type="button"
                  className="font-medium underline underline-offset-4"
                >
                  Add variables
                </button>
              </AlertDescription>
              <AlertAction>
                <Button type="button" variant="ghost" size="icon-sm">
                  <XIcon />
                  <span className="sr-only">Dismiss</span>
                </Button>
              </AlertAction>
            </Alert>
            <ServiceVariableGroupAttachmentsPanel state={state} />
          </>
        )}
        renderAfterList={() => (
          <>
            <InheritedVariableGroupVariablesSection
              items={inheritedVariables}
              duplicateKeyCounts={inheritedKeyCounts}
            />
            <Separator />
            <Collapsible>
              <CollapsibleTrigger
                render={(props, collapsibleState) => (
                  <Button type="button" variant="ghost" {...props}>
                    <ChevronRightIcon
                      data-icon="inline-start"
                      className={collapsibleState.open ? "rotate-90" : undefined}
                    />
                    {ployzManagedVariables.length} Ployz variables
                  </Button>
                )}
              />
              <CollapsibleContent>
                <div className="pt-2">
                  <p className="text-sm text-muted-foreground">
                    Ployz adds these system variables to every build and deploy.
                  </p>
                  <Table className="mt-4">
                    <TableBody>
                      {ployzManagedVariables.map((variable) => (
                        <TableRow key={variable.key}>
                          <TableCell>
                            <div className="flex items-center gap-1.5">
                              <div className="font-medium">{variable.key}</div>
                              <Tooltip>
                                <TooltipTrigger
                                  render={
                                    <Button
                                      type="button"
                                      variant="ghost"
                                      size="icon-sm"
                                    >
                                      <InfoIcon />
                                      <span className="sr-only">
                                        {variable.key} description
                                      </span>
                                    </Button>
                                  }
                                />
                                <TooltipContent>
                                  {variable.description}
                                </TooltipContent>
                              </Tooltip>
                            </div>
                          </TableCell>
                          <TableCell>
                            <SecretValueDisplay
                              value={variable.value}
                              info={`${variable.key} is a Ployz-managed system variable.`}
                            />
                          </TableCell>
                          <TableCell>
                            <Button type="button" variant="ghost" size="sm">
                              Reference
                            </Button>
                          </TableCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                </div>
              </CollapsibleContent>
            </Collapsible>
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
        collection={variablesCollection}
        valueTargets={valueTargets}
      />
    </TabsContent>
  );
}

function InheritedVariableGroupVariablesSection({
  items,
  duplicateKeyCounts,
}: {
  items: Array<{
    attachment: EnvironmentServiceVariableGroupAttachment;
    resourceName: string;
    variable: VariableRecord;
  }>;
  duplicateKeyCounts: Map<string, number>;
}) {
  if (items.length === 0) {
    return null;
  }

  return (
    <section className="flex flex-col gap-3">
      <div className="flex items-center gap-2">
        <Link2Icon className="size-4 text-muted-foreground" />
        <h2 className="font-medium">{items.length} Variable Group Variables</h2>
      </div>
      <Table>
        <TableBody>
          {items.map((item) => {
            const hasDuplicate = (duplicateKeyCounts.get(item.variable.key) ?? 0) > 1;
            return (
              <TableRow
                key={`${item.attachment.variableGroupId}:${item.variable.id}`}
              >
                <TableCell>
                  <div className="min-w-0">
                    <div className="truncate font-mono text-sm">
                      {item.variable.key}
                    </div>
                    <div className="truncate text-xs text-muted-foreground">
                      {item.resourceName}
                    </div>
                    {hasDuplicate ? (
                      <div className="mt-1 text-xs text-destructive">
                        Duplicate key across attached Variable Groups; later attachment
                        wins.
                      </div>
                    ) : null}
                  </div>
                </TableCell>
                <TableCell>
                  <SecretValueDisplay
                    value={
                      item.variable.value.type === "plain"
                        ? item.variable.value.value
                        : undefined
                    }
                    info={`${item.variable.key} comes from ${item.resourceName}.`}
                  />
                </TableCell>
                <TableCell className="text-right text-xs text-muted-foreground">
                  Read-only
                </TableCell>
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
    </section>
  );
}
