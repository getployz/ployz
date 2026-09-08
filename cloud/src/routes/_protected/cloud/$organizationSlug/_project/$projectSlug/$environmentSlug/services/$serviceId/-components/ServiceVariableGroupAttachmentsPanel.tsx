import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { useState } from "react";
import { eq, useLiveSuspenseQuery } from "@tanstack/react-db";
import { useServerFn } from "@tanstack/react-start";
import { DatabaseIcon, PackagePlusIcon, UnlinkIcon } from "lucide-react";
import { toast } from "sonner";
import { Button } from "#/components/ui/button";
import { Empty, EmptyHeader, EmptyTitle } from "#/components/ui/empty";
import { Separator } from "#/components/ui/separator";
import { Spinner } from "#/components/ui/spinner";
import { getEnvironmentsCollection } from "#/electric/collections";
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

type PendingAttachment = {
  action: "attach" | "detach";
  variableGroupId: string;
};

function pendingAttachmentKey(attachment: PendingAttachment) {
  return `${attachment.action}:${attachment.variableGroupId}`;
}

function VariableGroupAttachmentRow({
  variableGroup,
  action,
  pending,
  onClick,
}: {
  variableGroup: EnvironmentVariableGroupRecord;
  action: "attach" | "detach";
  pending: boolean;
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
        disabled={pending}
        onClick={onClick}
      >
        {pending ? (
          <Spinner />
        ) : action === "detach" ? (
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
  const attachVariableGroup = useServerFn(attachServiceVariableGroupServerFn);
  const detachVariableGroup = useServerFn(detachServiceVariableGroupServerFn);
  const rawAttachments = getEnvironmentsCollection(
    state.organizationSlug,
  );
  const environmentResourcesCollection = useEnvironmentResourcesCollection(
    state.organizationSlug,
  );
  const [pendingAttachment, setPendingAttachment] =
    useState<PendingAttachment | null>(null);

  const document = useEnvironmentDocument(state.organizationSlug, state.service.environmentId);
  const attachments = document?.intent.services.find((node) => node.id === state.service.id)?.variableGroupAttachments
    .map((attachment) => ({ ...attachment, serviceId: state.service.id, environmentId: state.service.environmentId })) ?? [];
  const { data: environmentResourceRows } = useLiveSuspenseQuery({
    query: (q) =>
      q
        .from({ resource: environmentResourcesCollection })
        .where(({ resource }) =>
          eq(resource.resource.environmentId, state.service.environmentId),
        )
        .select(({ resource }) => resource),
  });

  const pendingKey = pendingAttachment
    ? pendingAttachmentKey(pendingAttachment)
    : null;
  const environmentResources = environmentResourceRows.map((resource) =>
    parseLiveQueryRow(variableGroupResourceRecordSchema, resource),
  );
  const { attachedVariableGroups, availableVariableGroups } =
    getServiceVariableGroupAttachmentAvailability({
      attachments,
      environmentResources,
    });

  async function runAttachmentAction(attachment: PendingAttachment) {
    setPendingAttachment(attachment);
    try {
      if (!document) throw new Error("Environment is not loaded.");
      const data = {
        revision: document.revision,
        organizationSlug: state.organizationSlug,
        environmentId: state.service.environmentId,
        serviceId: state.service.id,
        variableGroupId: attachment.variableGroupId,
      };

      if (attachment.action === "attach") {
        const receipt = await attachVariableGroup({ data });
        await rawAttachments.utils.awaitTxId(receipt.txid);
      } else {
        const receipt = await detachVariableGroup({ data });
        await rawAttachments.utils.awaitTxId(receipt.txid);
      }
    } catch (error) {
      toast.error(
        error instanceof Error ? error.message : "Could not update Variable Groups.",
      );
    } finally {
      setPendingAttachment(null);
    }
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
                  pending={
                    pendingKey ===
                    pendingAttachmentKey({
                      action: "detach",
                      variableGroupId: variableGroup.id,
                    })
                  }
                  onClick={() =>
                    void runAttachmentAction({
                      action: "detach",
                      variableGroupId: variableGroup.id,
                    })
                  }
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
                  pending={
                    pendingKey ===
                    pendingAttachmentKey({
                      action: "attach",
                      variableGroupId: variableGroup.id,
                    })
                  }
                  onClick={() =>
                    void runAttachmentAction({
                      action: "attach",
                      variableGroupId: variableGroup.id,
                    })
                  }
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
