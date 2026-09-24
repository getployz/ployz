import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { captureEnvironmentNodeIntroduction } from "./environment-node-introduction.repository.server";
import type { Actor } from "#/modules/identity/actor";
import { withMutationResult } from "#/server/mutation-result.server";
import { slugifySegment } from "#/utils/slug";
import { getDuplicateEnvironmentNodeNameMessage, isEnvironmentNodeNameTaken, resolveUniqueEnvironmentNodeName } from "./environment-node-names";
import { environmentDesignFields } from "./fields";
import { listEnvironmentNodeNameIdentities, requireEnvironmentForActorById } from "./authoring-repository.server";
import { createResourceIdentity, getResourceIdentity, getVolumeResource, upsertResourceCanvasPosition } from "./resource-repository.server";
import { loadEnvironmentDocument, requireDocumentRevision, writeEnvironmentDocument } from "./working-state-repository.server";
import type { CreateVolumeResourceInput, DeleteVolumeResourceInput, UpdateEnvironmentResourceCanvasPositionInput, UpdateVolumeResourceInput } from "./resources";
import { Conflict, NotFound } from "#/server/public-error";

export const createVolumeResource = Effect.fn("EnvironmentDesign.createVolumeResource")(
  function* (actor: Actor, input: CreateVolumeResourceInput) {
    const context = yield* requireEnvironmentForActorById(actor, input);
    const receipt = yield* withMutationResult(Effect.gen(function* () {
      const document = yield* loadEnvironmentDocument(input.environmentId, true);
      const name = resolveUniqueEnvironmentNodeName({ name: input.name, nodes: yield* listEnvironmentNodeNameIdentities(input.environmentId), schema: environmentDesignFields.resource.name, maxLength: 64 });
      const slug = slugifySegment(name) || "volume";
      const { resource, lineage, canvasPosition } = yield* createResourceIdentity({ ...input, projectId: context.project.id, name, slug });
      document.intent.volumes.push({ resourceId: resource.id, resourceLineageId: resource.lineageId, name });
      const environment = yield* writeEnvironmentDocument(document, document.intent);
      const introduction = yield* captureEnvironmentNodeIntroduction({ environmentId: input.environmentId, nodeType: "volume", nodeId: resource.id });
      return { resource, lineage, canvasPosition, environment, introduction };
    }));
    const data = yield* getVolumeResource(input.environmentId, receipt.data.resource.id);
    if (!data) return yield* new NotFound({ message: "Volume not found." });
    return { ...receipt.data, data };
  },
);

/** Renames the Volume when `name` is present; otherwise removes it and its mounts. */
const editVolume = Effect.fn("EnvironmentDesign.editVolume")(
  function* (actor: Actor, input: DeleteVolumeResourceInput & { name?: string }) {
    yield* requireEnvironmentForActorById(actor, input);
    return yield* withMutationResult(Effect.gen(function* () {
      const document = yield* loadEnvironmentDocument(input.environmentId, true);
      yield* requireDocumentRevision(document, input.revision);
      const node = document.intent.volumes.find((node) => node.resourceId === input.resourceId);
      if (!node) return yield* new NotFound({ message: "Resource not found." });
      if (input.name !== undefined) {
        if (isEnvironmentNodeNameTaken(input.name, yield* listEnvironmentNodeNameIdentities(input.environmentId), { type: "volume", id: input.resourceId })) return yield* new Conflict({ message: getDuplicateEnvironmentNodeNameMessage(input.name) });
        node.name = input.name;
      } else {
        document.intent.volumes = document.intent.volumes.filter((node) => node.resourceId !== input.resourceId);
        for (const service of document.intent.services) service.volumeAttachments = service.volumeAttachments.filter((mount) => mount.volumeResourceId !== input.resourceId);
      }
      return yield* writeEnvironmentDocument(document, document.intent);
    }));
  },
);

export const updateVolumeResource: (actor: Actor, input: UpdateVolumeResourceInput) => ReturnType<typeof editVolume> = editVolume;
export const deleteVolumeResource: (actor: Actor, input: DeleteVolumeResourceInput) => ReturnType<typeof editVolume> = editVolume;

export const updateEnvironmentResourceCanvasPosition = Effect.fn(
  "EnvironmentDesign.updateEnvironmentResourceCanvasPosition",
)(function* (
  actor: Actor,
  input: UpdateEnvironmentResourceCanvasPositionInput,
) {
  yield* requireEnvironmentForActorById(actor, input);
  return yield* withMutationResult(Effect.gen(function* () {
    yield* loadEnvironmentDocument(input.environmentId, true);
    const resource = yield* getResourceIdentity(input.environmentId, input.resourceId);
    if (resource === null) return yield* new NotFound({ message: "Resource not found." });
    return yield* upsertResourceCanvasPosition({ ...input, resourceType: resource.implementationType });
  }));
});
