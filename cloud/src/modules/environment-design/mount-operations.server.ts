import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { withMutationReceipt } from "#/server/mutation-receipt.server";
import { Conflict, NotFound, Validation } from "#/server/public-error";
import { requireEnvironmentForActorById } from "./authoring-repository.server";
import {
  deleteServiceMount,
  getMountTarget,
  insertServiceMount,
  listServiceMounts,
  updateServiceMount,
} from "./mount-repository.server";
import {
  getAttachmentTargetError,
  getMountConflict,
  type AttachServiceVolumeInput,
  type DetachServiceVolumeInput,
  type UpdateServiceVolumeMountPathInput,
} from "./service-volume-attachments";

const requireMountTarget = Effect.fn("EnvironmentDesign.requireMountTarget")(
  function* (
    actor: Actor,
    input: AttachServiceVolumeInput,
  ) {
    const context = yield* requireEnvironmentForActorById(actor, input);
    const target = yield* getMountTarget(
      input.serviceId,
      input.volumeResourceId,
    );
    if (target.service === null) {
      return yield* new NotFound({ message: "Service not found." });
    }
    if (target.volume === null) {
      return yield* new NotFound({ message: "Volume not found." });
    }
    if (target.volume.deletedAt !== null) {
      return yield* new Conflict({
        message: "This volume is staged for deletion.",
      });
    }
    const targetError = getAttachmentTargetError({
      serviceEnvironmentId: target.service.environmentId,
      resourceEnvironmentId: target.volume.environmentId,
      resourceImplementationType: target.volume.implementationType,
    });
    if (targetError === "cross_environment") {
      return yield* new Validation({
        message: "A service can only mount volumes in its own environment.",
      });
    }
    if (targetError === "non_volume_resource") {
      return yield* new Validation({
        message: "That resource is not a volume.",
      });
    }
    if (
      target.service.environmentId !== input.environmentId ||
      target.volume.environmentId !== input.environmentId
    ) {
      return yield* new Validation({
        message: "Service and volume must belong to the requested environment.",
      });
    }
    return context;
  },
);

function mountConflict(input: {
  readonly serviceMounts: ReadonlyArray<{
    readonly volumeResourceId: string;
    readonly mountPath: string;
  }>;
  readonly volumeResourceId: string;
  readonly mountPath: string;
  readonly mode: "attach" | "edit";
}) {
  const conflict = getMountConflict({
    ...input,
    serviceMounts: [...input.serviceMounts],
  });
  if (conflict === null) return null;
  return new Conflict({
    message:
      conflict.type === "duplicate_pair"
        ? "This volume is already mounted on the service."
        : `Another volume is already mounted at ${conflict.mountPath}.`,
  });
}

export const attachServiceVolume = Effect.fn(
  "EnvironmentDesign.attachServiceVolume",
)(function* (actor: Actor, input: AttachServiceVolumeInput) {
  const context = yield* requireMountTarget(actor, input);
  const serviceMounts = yield* listServiceMounts(input.serviceId);
  const conflict = mountConflict({
    serviceMounts,
    volumeResourceId: input.volumeResourceId,
    mountPath: input.mountPath,
    mode: "attach",
  });
  if (conflict !== null) return yield* conflict;
  const receipt = yield* withMutationReceipt(
    insertServiceMount({
      projectId: context.project.id,
      environmentId: input.environmentId,
      serviceId: input.serviceId,
      volumeResourceId: input.volumeResourceId,
      mountPath: input.mountPath,
    }),
  );
  if (receipt.data === null) {
    return yield* new Conflict({
      message: "The volume mount could not be created.",
    });
  }
  return { ...receipt, data: receipt.data };
});

export const updateServiceVolumeMountPath = Effect.fn(
  "EnvironmentDesign.updateServiceVolumeMountPath",
)(function* (actor: Actor, input: UpdateServiceVolumeMountPathInput) {
  yield* requireMountTarget(actor, input);
  const serviceMounts = yield* listServiceMounts(input.serviceId);
  if (
    !serviceMounts.some(
      (mount) => mount.volumeResourceId === input.volumeResourceId,
    )
  ) {
    return yield* new NotFound({ message: "Volume not found." });
  }
  const conflict = mountConflict({
    serviceMounts,
    volumeResourceId: input.volumeResourceId,
    mountPath: input.mountPath,
    mode: "edit",
  });
  if (conflict !== null) return yield* conflict;
  const receipt = yield* withMutationReceipt(updateServiceMount(input));
  if (receipt.data === null) {
    return yield* new NotFound({ message: "Volume not found." });
  }
  return { ...receipt, data: receipt.data };
});

export const detachServiceVolume = Effect.fn(
  "EnvironmentDesign.detachServiceVolume",
)(function* (actor: Actor, input: DetachServiceVolumeInput) {
  yield* requireEnvironmentForActorById(actor, input);
  return yield* withMutationReceipt(deleteServiceMount(input));
});
