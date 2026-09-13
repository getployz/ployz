import { useCollectionScope } from "#/collections/use-collection-scope";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { useReducer } from "react";
import { HardDriveIcon } from "lucide-react";
import { toast } from "sonner";
import { useServerFn } from "@tanstack/react-start";
import { getEnvironmentsCollection } from "#/collections/collections";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "#/components/ui/empty";
import {
  attachServiceVolumeServerFn,
  detachServiceVolumeServerFn,
  updateServiceVolumeMountPathServerFn,
} from "#/modules/environment-design/resource-functions";
import {
  getMountConflict,
  mountPathSchema,
  type ServiceMount,
} from "#/modules/environment-design/service-volume-attachments";
import { Result, Schema } from "effect";
import { strictParseOptions } from "#/modules/environment-design/schema";
import { VolumeMountForm } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/VolumeMountForm";
import { VolumeMountItem } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/VolumeMountItem";
import type { VolumeDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/useVolumeDrawerState";

function conflictMessage(
  serviceMounts: ServiceMount[],
  volumeResourceId: string,
  mountPath: string,
  mode: "attach" | "edit",
) {
  const conflict = getMountConflict({
    serviceMounts,
    volumeResourceId,
    mountPath,
    mode,
  });
  if (!conflict) return null;
  return conflict.type === "duplicate_pair"
    ? "This volume is already mounted on the service."
    : `Another volume is already mounted at ${conflict.mountPath}.`;
}

type VolumeAttachmentsState = {
  addServiceId: string;
  addMountPath: string;
  addError: string | null;
  editingServiceId: string | null;
  editPath: string;
  editError: string | null;
  pending: boolean;
};

type VolumeAttachmentsAction =
  | { type: "patch"; patch: Partial<VolumeAttachmentsState> }
  | { type: "resetAddForm" }
  | { type: "startEdit"; serviceId: string; mountPath: string }
  | { type: "cancelEdit" };

const initialVolumeAttachmentsState: VolumeAttachmentsState = {
  addServiceId: "",
  addMountPath: "/data",
  addError: null,
  editingServiceId: null,
  editPath: "",
  editError: null,
  pending: false,
};

function volumeAttachmentsReducer(
  state: VolumeAttachmentsState,
  action: VolumeAttachmentsAction,
): VolumeAttachmentsState {
  switch (action.type) {
    case "patch":
      return { ...state, ...action.patch };
    case "resetAddForm":
      return {
        ...state,
        addServiceId: "",
        addMountPath: "/data",
        addError: null,
      };
    case "startEdit":
      return {
        ...state,
        editingServiceId: action.serviceId,
        editPath: action.mountPath,
        editError: null,
      };
    case "cancelEdit":
      return { ...state, editingServiceId: null, editError: null };
  }
}

export function VolumeAttachmentsTab({ state }: { state: VolumeDrawerState }) {
  const collectionScope = useCollectionScope();
  const attachVolume = useServerFn(attachServiceVolumeServerFn);
  const detachVolume = useServerFn(detachServiceVolumeServerFn);
  const updateMountPath = useServerFn(updateServiceVolumeMountPathServerFn);
  const attachmentsCollection = getEnvironmentsCollection(state.organizationSlug, collectionScope);

  const document = useEnvironmentDocument(state.organizationSlug, state.environmentId);
  function revision() {
    if (!document) throw new Error("Environment is not loaded.");
    return document.revision;
  }
  const volumeResourceId = state.resource.resource.id;
  const isRemoved = !state.resource.isAuthored;
  const serviceNameById = new Map(
    state.services.map((service) => [service.id, service.name]),
  );
  const mounts = state.attachments
    .filter((attachment) => attachment.volumeResourceId === volumeResourceId)
    .sort((left, right) => left.mountPath.localeCompare(right.mountPath));
  const attachedServiceIds = new Set(mounts.map((mount) => mount.serviceId));
  const availableServices = state.services.filter(
    (service) => !attachedServiceIds.has(service.id),
  );

  const [mountState, dispatchMountState] = useReducer(
    volumeAttachmentsReducer,
    initialVolumeAttachmentsState,
  );

  function serviceMountsFor(serviceId: string): ServiceMount[] {
    return state.attachments.flatMap((attachment) =>
      attachment.serviceId === serviceId
        ? [{
            volumeResourceId: attachment.volumeResourceId,
            mountPath: attachment.mountPath,
          }]
        : [],
    );
  }

  async function handleAttach() {
    if (!mountState.addServiceId) {
      dispatchMountState({
        type: "patch",
        patch: { addError: "Select a service." },
      });
      return;
    }
    const parsed = Schema.decodeUnknownResult(mountPathSchema)(
      mountState.addMountPath,
      strictParseOptions,
    );
    if (Result.isFailure(parsed)) {
      dispatchMountState({
        type: "patch",
        patch: {
          addError:
            parsed.failure instanceof Error
              ? parsed.failure.message
              : "Invalid value",
        },
      });
      return;
    }
    const conflict = conflictMessage(
      serviceMountsFor(mountState.addServiceId),
      volumeResourceId,
      parsed.success,
      "attach",
    );
    if (conflict) {
      dispatchMountState({ type: "patch", patch: { addError: conflict } });
      return;
    }

    dispatchMountState({ type: "patch", patch: { pending: true } });
    try {
      const result = await attachVolume({
        data: {
          organizationSlug: state.organizationSlug,
          environmentId: state.environmentId,
          revision: revision(),
          serviceId: mountState.addServiceId,
          volumeResourceId,
          mountPath: parsed.success,
        },
      });
      await attachmentsCollection.writeCommitted(result.data);
      dispatchMountState({ type: "resetAddForm" });
    } catch (error) {
      dispatchMountState({
        type: "patch",
        patch: {
          addError:
            error instanceof Error
              ? error.message
              : "Failed to mount the volume.",
        },
      });
    } finally {
      dispatchMountState({ type: "patch", patch: { pending: false } });
    }
  }

  async function handleSaveEdit(serviceId: string) {
    const parsed = Schema.decodeUnknownResult(mountPathSchema)(
      mountState.editPath,
      strictParseOptions,
    );
    if (Result.isFailure(parsed)) {
      dispatchMountState({
        type: "patch",
        patch: {
          editError:
            parsed.failure instanceof Error
              ? parsed.failure.message
              : "Invalid value",
        },
      });
      return;
    }
    const conflict = conflictMessage(
      serviceMountsFor(serviceId),
      volumeResourceId,
      parsed.success,
      "edit",
    );
    if (conflict) {
      dispatchMountState({ type: "patch", patch: { editError: conflict } });
      return;
    }

    dispatchMountState({ type: "patch", patch: { pending: true } });
    try {
      const result = await updateMountPath({
        data: {
          organizationSlug: state.organizationSlug,
          environmentId: state.environmentId,
          revision: revision(),
          serviceId,
          volumeResourceId,
          mountPath: parsed.success,
        },
      });
      await attachmentsCollection.writeCommitted(result.data);
      dispatchMountState({ type: "cancelEdit" });
    } catch (error) {
      dispatchMountState({
        type: "patch",
        patch: {
          editError:
            error instanceof Error
              ? error.message
              : "Failed to update the mount.",
        },
      });
    } finally {
      dispatchMountState({ type: "patch", patch: { pending: false } });
    }
  }

  async function handleDetach(serviceId: string) {
    dispatchMountState({ type: "patch", patch: { pending: true } });
    try {
      const result = await detachVolume({
        data: {
          organizationSlug: state.organizationSlug,
          environmentId: state.environmentId,
          revision: revision(),
          serviceId,
          volumeResourceId,
        },
      });
      await attachmentsCollection.writeCommitted(result.data);
    } catch (error) {
      toast.error(
        error instanceof Error ? error.message : "Failed to remove the mount.",
      );
    } finally {
      dispatchMountState({ type: "patch", patch: { pending: false } });
    }
  }

  if (isRemoved) {
    return (
      <Empty>
        <EmptyHeader>
          <EmptyMedia variant="icon">
            <HardDriveIcon />
          </EmptyMedia>
          <EmptyTitle>Volume staged for deletion</EmptyTitle>
          <EmptyDescription>
            Discard the delete from the staged changes to manage mounts again.
          </EmptyDescription>
        </EmptyHeader>
      </Empty>
    );
  }

  return (
    <div className="flex flex-col gap-6">
      {mounts.length === 0 ? (
        <Empty>
          <EmptyHeader>
            <EmptyMedia variant="icon">
              <HardDriveIcon />
            </EmptyMedia>
            <EmptyTitle>No mounts yet</EmptyTitle>
            <EmptyDescription>
              Mount this volume on a service to give it persistent storage.
            </EmptyDescription>
          </EmptyHeader>
        </Empty>
      ) : (
        <div className="flex flex-col gap-2">
          {mounts.map((mount) => {
            const isEditing = mountState.editingServiceId === mount.serviceId;
            return (
              <VolumeMountItem
                key={mount.serviceId}
                mount={mount}
                serviceName={
                  serviceNameById.get(mount.serviceId) ?? "Unknown service"
                }
                editError={mountState.editError}
                editPath={mountState.editPath}
                isEditing={isEditing}
                pending={mountState.pending}
                onCancelEdit={() => dispatchMountState({ type: "cancelEdit" })}
                onDetach={() => void handleDetach(mount.serviceId)}
                onEditPathChange={(value) =>
                  dispatchMountState({
                    type: "patch",
                    patch: { editPath: value, editError: null },
                  })
                }
                onSaveEdit={() => void handleSaveEdit(mount.serviceId)}
                onStartEdit={() =>
                  dispatchMountState({
                    type: "startEdit",
                    serviceId: mount.serviceId,
                    mountPath: mount.mountPath,
                  })
                }
              />
            );
          })}
        </div>
      )}

      <VolumeMountForm
        addError={mountState.addError}
        addMountPath={mountState.addMountPath}
        addServiceId={mountState.addServiceId}
        availableServices={availableServices}
        pending={mountState.pending}
        onAttach={() => void handleAttach()}
        onMountPathChange={(value) =>
          dispatchMountState({
            type: "patch",
            patch: { addMountPath: value, addError: null },
          })
        }
        onServiceChange={(value) =>
          dispatchMountState({
            type: "patch",
            patch: { addServiceId: value ?? "", addError: null },
          })
        }
      />
    </div>
  );
}
