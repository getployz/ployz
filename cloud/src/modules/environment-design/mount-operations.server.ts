import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { withMutationReceipt } from "#/server/mutation-receipt.server";
import { Conflict, NotFound } from "#/server/public-error";
import { requireEnvironmentForActorById } from "./authoring-repository.server";
import { loadEnvironmentDocument, requireDocumentRevision, writeEnvironmentDocument } from "./working-state-repository.server";
import { getMountConflict, type AttachServiceVolumeInput, type DetachServiceVolumeInput, type UpdateServiceVolumeMountPathInput } from "./service-volume-attachments";

const editMount = Effect.fn("EnvironmentDesign.editMount")(
  function* (actor: Actor, input: DetachServiceVolumeInput & { mountPath?: string }, mode: "attach" | "edit" | "detach") {
    yield* requireEnvironmentForActorById(actor, input);
    return yield* withMutationReceipt(Effect.gen(function* () {
      const document = yield* loadEnvironmentDocument(input.environmentId, true);
      yield* requireDocumentRevision(document, input.revision);
      const node = document.intent.services.find((node) => node.id === input.serviceId);
      if (!node) return yield* new NotFound({ message: "Service not found." });
      if (!document.intent.volumes.some((node) => node.resourceId === input.volumeResourceId)) return yield* new NotFound({ message: "Volume not found." });
      const current = node.volumeAttachments.find((mount) => mount.volumeResourceId === input.volumeResourceId);
      if (mode === "detach") node.volumeAttachments = node.volumeAttachments.filter((mount) => mount.volumeResourceId !== input.volumeResourceId);
      else {
        if (mode === "edit" && !current) return yield* new NotFound({ message: "Volume mount not found." });
        if (input.mountPath === undefined) return yield* new Conflict({ message: "Mount path is required." });
        const conflict = getMountConflict({ serviceMounts: node.volumeAttachments, volumeResourceId: input.volumeResourceId, mountPath: input.mountPath, mode });
        if (conflict) return yield* new Conflict({ message: conflict.type === "duplicate_pair" ? "This volume is already mounted on the service." : `Another volume is already mounted at ${conflict.mountPath}.` });
        if (current) current.mountPath = input.mountPath;
        else node.volumeAttachments.push({ volumeResourceId: input.volumeResourceId, mountPath: input.mountPath });
      }
      return yield* writeEnvironmentDocument(document, document.intent);
    }));
  },
);

export const attachServiceVolume = Effect.fn("EnvironmentDesign.attachServiceVolume")(
  (actor: Actor, input: AttachServiceVolumeInput) => editMount(actor, input, "attach"),
);
export const updateServiceVolumeMountPath = Effect.fn("EnvironmentDesign.updateServiceVolumeMountPath")(
  (actor: Actor, input: UpdateServiceVolumeMountPathInput) => editMount(actor, input, "edit"),
);
export const detachServiceVolume = Effect.fn("EnvironmentDesign.detachServiceVolume")(
  (actor: Actor, input: DetachServiceVolumeInput) => editMount(actor, input, "detach"),
);
