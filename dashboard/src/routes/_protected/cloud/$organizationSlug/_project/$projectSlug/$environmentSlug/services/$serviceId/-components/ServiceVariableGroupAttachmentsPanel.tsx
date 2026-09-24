import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { useEnvironmentDocumentEditor } from "#/modules/environment-design/environment-document-edit";
import { eq, useLiveSuspenseQuery } from "@tanstack/react-db";
import { useServerFn } from "@tanstack/react-start";
import { DatabaseIcon, PackagePlusIcon, UnlinkIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { Empty, EmptyHeader, EmptyTitle } from "#/components/ui/empty";
import { Separator } from "#/components/ui/separator";
import { parseLiveQueryRow } from "#/lib/tanstack-db";
import {
  attachServiceVariableGroupServerFn,
  detachServiceVariableGroupServerFn,
} from "#/modules/environment-design/variable-functions";
import {
  type EnvironmentVariableGroupRecord,
} from "#/modules/environment-design/variables";
import { variableGroupResourceRecordSchema } from "#/modules/environment-design/resources";
import {
  getServiceVariableGroupAttachmentAvailability,
} from "#/modules/environment-design/service-variable-group-attachments";
import {
  useEnvironmentResourcesCollection,
} from "#/modules/services/services.collection";
import type { ServiceDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";

function VariableGroupAttachmentRow({
  variableGroup,
  action,
  onClick,
}: {
  variableGroup: EnvironmentVariableGroupRecord;
  action: "attach" | "detach";
  onClick: () => void;
}) {
  return (
    <div className="flex items-center justify-between gap-3 border-b py-2 last:border-b-0">
      <div className="min-w-0">
        <div className="truncate font-medium">{variableGroup.name}</div>
        <div className="truncate font-mono text-xs text-muted-foreground">
          {variableGroup.slug}
        </div>
      </div>
      <Button
        type="button"
        variant={action === "detach" ? "ghost" : "outline"}
        size="sm"
        onClick={onClick}
      >
        {action === "detach" ? (
          <UnlinkIcon data-icon="inline-start" />
        ) : (
          <PackagePlusIcon data-icon="inline-start" />
        )}
        {action === "detach" ? "Detach" : "Attach"}
      </Button>
    </div>
  );
}

export function ServiceVariableGroupAttachmentsPanel({
  state,
}: {
  state: ServiceDrawerState;
}) {
  const editDocument = useEnvironmentDocumentEditor(state.organizationSlug);
  const attachVariableGroup = useServerFn(attachServiceVariableGroupServerFn);
  const detachVariableGroup = useServerFn(detachServiceVariableGroupServerFn);
  const environmentResourcesCollection = useEnvironmentResourcesCollection(
    state.organizationSlug,
  );
  const document = useEnvironmentDocument(state.organizationSlug, state.service.environmentId);
  const attachments = document?.intent.services.find((node) => node.id === state.service.id)?.variableGroupAttachments
    .map((attachment) => ({ ...attachment, serviceId: state.service.id, environmentId: state.service.environmentId })) ?? [];
  const { data: environmentResourceRows } = useLiveSuspenseQuery({
    queryKey: ['service-attachment-resources', environmentResourcesCollection.id, state.service.environmentId],
    query: (q) =>
      q
        .from({ resource: environmentResourcesCollection })
        .where(({ resource }) =>
          eq(resource.resource.environmentId, state.service.environmentId),
        )
        .select(({ resource }) => resource),
  });

  const environmentResources = environmentResourceRows.map((resource) =>
    parseLiveQueryRow(variableGroupResourceRecordSchema, resource),
  );
  const { attachedVariableGroups, availableVariableGroups } =
    getServiceVariableGroupAttachmentAvailability({
      attachments,
      environmentResources,
    });

  function runAttachmentAction(action: "attach" | "detach", variableGroupId: string) {
    const { organizationSlug } = state;
    const { environmentId, id: serviceId } = state.service;
    editDocument({
      environmentId,
      apply: (intent) => {
        const node = intent.services.find((node) => node.id === serviceId);
        if (!node) return;
        if (action === "detach") {
          node.variableGroupAttachments = node.variableGroupAttachments.filter((attachment) => attachment.variableGroupId !== variableGroupId);
          return;
        }
        const sortOrder = Math.max(-1, ...node.variableGroupAttachments.map((attachment) => attachment.sortOrder)) + 1;
        node.variableGroupAttachments.push({ variableGroupId, sortOrder });
      },
      save: (revision) => {
        const data = { revision, organizationSlug, environmentId, serviceId, variableGroupId };
        return action === "attach" ? attachVariableGroup({ data }) : detachVariableGroup({ data });
      },
      failureMessage: "Could not update Variable Groups.",
    });
  }

  return (
    <section className="flex flex-col gap-4">
      <div className="flex items-center gap-2">
        <DatabaseIcon className="size-4 text-muted-foreground" />
        <h2 className="font-medium">Variable Groups</h2>
      </div>

      <div className="grid gap-4 lg:grid-cols-2">
        <div className="min-w-0">
          <h3 className="mb-2 text-sm font-medium">Attached</h3>
          {attachedVariableGroups.length > 0 ? (
            <div className="flex flex-col">
              {attachedVariableGroups.map((variableGroup) => (
                <VariableGroupAttachmentRow
                  key={variableGroup.id}
                  variableGroup={variableGroup}
                  action="detach"
                  onClick={() => runAttachmentAction("detach", variableGroup.id)}
                />
              ))}
            </div>
          ) : (
            <Empty>
              <EmptyHeader>
                <EmptyTitle>No Variable Groups attached</EmptyTitle>
              </EmptyHeader>
            </Empty>
          )}
        </div>

        <div className="min-w-0">
          <h3 className="mb-2 text-sm font-medium">Available</h3>
          {availableVariableGroups.length > 0 ? (
            <div className="flex flex-col">
              {availableVariableGroups.map((variableGroup) => (
                <VariableGroupAttachmentRow
                  key={variableGroup.id}
                  variableGroup={variableGroup}
                  action="attach"
                  onClick={() => runAttachmentAction("attach", variableGroup.id)}
                />
              ))}
            </div>
          ) : (
            <Empty>
              <EmptyHeader>
                <EmptyTitle>No Variable Groups available</EmptyTitle>
              </EmptyHeader>
            </Empty>
          )}
        </div>
      </div>

      <Separator />
    </section>
  );
}
