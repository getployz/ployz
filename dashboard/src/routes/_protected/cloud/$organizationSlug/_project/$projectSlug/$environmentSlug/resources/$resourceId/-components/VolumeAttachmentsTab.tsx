import { useEnvironmentDocumentEditor } from "#/modules/environment-design/environment-document-edit";
import { useReducer } from "react";
import { HardDriveIcon } from "lucide-react";
import { useServerFn } from "@tanstack/react-start";
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
  const editDocument = useEnvironmentDocumentEditor(state.organizationSlug);
  const attachVolume = useServerFn(attachServiceVolumeServerFn);
  const detachVolume = useServerFn(detachServiceVolumeServerFn);
  const updateMountPath = useServerFn(updateServiceVolumeMountPathServerFn);
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

  function handleAttach() {
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

    const serviceId = mountState.addServiceId;
    const mountPath = parsed.success;
    editDocument({
      environmentId: state.environmentId,
      apply: (intent) => {
        intent.services.find((node) => node.id === serviceId)?.volumeAttachments.push({ volumeResourceId, mountPath });
      },
      save: (revision) => attachVolume({ data: {
        organizationSlug: state.organizationSlug, environmentId: state.environmentId, revision, serviceId, volumeResourceId, mountPath,
      } }),
      failureMessage: "Failed to mount the volume.",
    });
    dispatchMountState({ type: "resetAddForm" });
  }

  function handleSaveEdit(serviceId: string) {
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

    const mountPath = parsed.success;
    editDocument({
      environmentId: state.environmentId,
      apply: (intent) => {
        const mount = intent.services.find((node) => node.id === serviceId)?.volumeAttachments
          .find((attachment) => attachment.volumeResourceId === volumeResourceId);
        if (mount) mount.mountPath = mountPath;
      },
      save: (revision) => updateMountPath({ data: {
        organizationSlug: state.organizationSlug, environmentId: state.environmentId, revision, serviceId, volumeResourceId, mountPath,
      } }),
      failureMessage: "Failed to update the mount.",
    });
    dispatchMountState({ type: "cancelEdit" });
  }

  function handleDetach(serviceId: string) {
    editDocument({
      environmentId: state.environmentId,
      apply: (intent) => {
        const node = intent.services.find((node) => node.id === serviceId);
        if (node) node.volumeAttachments = node.volumeAttachments.filter((attachment) => attachment.volumeResourceId !== volumeResourceId);
      },
      save: (revision) => detachVolume({ data: {
        organizationSlug: state.organizationSlug, environmentId: state.environmentId, revision, serviceId, volumeResourceId,
      } }),
      failureMessage: "Failed to remove the mount.",
    });
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
                onCancelEdit={() => dispatchMountState({ type: "cancelEdit" })}
                onDetach={() => handleDetach(mount.serviceId)}
                onEditPathChange={(value) =>
                  dispatchMountState({
                    type: "patch",
                    patch: { editPath: value, editError: null },
                  })
                }
                onSaveEdit={() => handleSaveEdit(mount.serviceId)}
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
        onAttach={() => handleAttach()}
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
