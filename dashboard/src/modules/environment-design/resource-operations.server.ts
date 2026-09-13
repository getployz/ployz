import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { captureEnvironmentNodeIntroduction } from "./environment-node-introduction.repository.server";
import type { Actor } from "#/modules/identity/actor";
import { withMutationResult } from "#/server/mutation-result.server";
import { slugifySegment } from "#/utils/slug";
import { getDuplicateEnvironmentNodeNameMessage, isEnvironmentNodeNameTaken, resolveUniqueEnvironmentNodeName } from "./environment-node-names";
import { environmentDesignFields } from "./fields";
import { listEnvironmentNodeNameIdentities, requireEnvironmentForActorById } from "./authoring-repository.server";
import { createResourceIdentity, getResourceIdentity, getVariableGroupResource, getVolumeResource, upsertResourceCanvasPosition } from "./resource-repository.server";
import { loadEnvironmentDocument, requireDocumentRevision, writeEnvironmentDocument } from "./working-state-repository.server";
import type { CreateVariableGroupResourceInput, CreateVolumeResourceInput, DeleteVariableGroupResourcePlanInput, DeleteVolumeResourceInput, UpdateEnvironmentResourceCanvasPositionInput, UpdateVariableGroupResourceInput, UpdateVolumeResourceInput } from "./resources";
import { Conflict, NotFound } from "#/server/public-error";

const createResource = Effect.fn("EnvironmentDesign.createResource")(
  function* (actor: Actor, input: CreateVolumeResourceInput, type: "variable_group" | "volume") {
    const context = yield* requireEnvironmentForActorById(actor, input);
    return yield* withMutationResult(Effect.gen(function* () {
      const document = yield* loadEnvironmentDocument(input.environmentId, true);
      const name = resolveUniqueEnvironmentNodeName({ name: input.name, nodes: yield* listEnvironmentNodeNameIdentities(input.environmentId), schema: environmentDesignFields.resource.name, maxLength: 64 });
      const slug = slugifySegment(name) || (type === "volume" ? "volume" : "variable-group");
      const { resource, group, lineage, canvasPosition } = yield* createResourceIdentity({ ...input, projectId: context.project.id, name, slug, type });
      if (group) document.intent.variableGroups.push({ resourceId: resource.id, resourceLineageId: resource.lineageId, variableGroupId: group.id, variableGroupLineageId: group.lineageId, name, slug, variables: [] });
      else document.intent.volumes.push({ resourceId: resource.id, resourceLineageId: resource.lineageId, name });
      const environment = yield* writeEnvironmentDocument(document, document.intent);
      const introduction = yield* captureEnvironmentNodeIntroduction({ environmentId: input.environmentId, nodeType: type, nodeId: resource.id });
      return { resource, lineage, canvasPosition, environment, introduction };
    }));
  },
);

export const createVariableGroupResource = Effect.fn("EnvironmentDesign.createVariableGroupResource")(
  function* (actor: Actor, input: CreateVariableGroupResourceInput) {
    const receipt = yield* createResource(actor, input, "variable_group");
    const data = yield* getVariableGroupResource(input.environmentId, receipt.data.resource.id);
    if (!data) return yield* new NotFound({ message: "Variable group not found." });
    return { ...receipt.data, data };
  },
);
export const createVolumeResource = Effect.fn("EnvironmentDesign.createVolumeResource")(
  function* (actor: Actor, input: CreateVolumeResourceInput) {
    const receipt = yield* createResource(actor, input, "volume");
    const data = yield* getVolumeResource(input.environmentId, receipt.data.resource.id);
    if (!data) return yield* new NotFound({ message: "Volume not found." });
    return { ...receipt.data, data };
  },
);

const editResource = Effect.fn("EnvironmentDesign.editResource")(
  function* (actor: Actor, input: DeleteVolumeResourceInput & { name?: string }, type: "variable_group" | "volume") {
    yield* requireEnvironmentForActorById(actor, input);
    return yield* withMutationResult(Effect.gen(function* () {
      const document = yield* loadEnvironmentDocument(input.environmentId, true);
      yield* requireDocumentRevision(document, input.revision);
      const node = (type === "volume" ? document.intent.volumes : document.intent.variableGroups).find((node) => node.resourceId === input.resourceId);
      if (!node) return yield* new NotFound({ message: "Resource not found." });
      if (input.name !== undefined) {
        if (isEnvironmentNodeNameTaken(input.name, yield* listEnvironmentNodeNameIdentities(input.environmentId), { type, id: input.resourceId })) return yield* new Conflict({ message: getDuplicateEnvironmentNodeNameMessage(input.name) });
        node.name = input.name;
      } else if (type === "volume") {
        document.intent.volumes = document.intent.volumes.filter((node) => node.resourceId !== input.resourceId);
        for (const service of document.intent.services) service.volumeAttachments = service.volumeAttachments.filter((mount) => mount.volumeResourceId !== input.resourceId);
      } else {
        const group = document.intent.variableGroups.find((node) => node.resourceId === input.resourceId);
        if (group && document.intent.services.some((service) => service.variableGroupAttachments.some((attachment) => attachment.variableGroupId === group.variableGroupId))) return yield* new Conflict({ message: "Variable Group has consumers and must be replaced or disconnected first." });
        document.intent.variableGroups = document.intent.variableGroups.filter((node) => node.resourceId !== input.resourceId);
      }
      return yield* writeEnvironmentDocument(document, document.intent);
    }));
  },
);

export const updateVariableGroupResource = Effect.fn("EnvironmentDesign.updateVariableGroupResource")(
  (actor: Actor, input: UpdateVariableGroupResourceInput) => editResource(actor, input, "variable_group"),
);
export const deleteVariableGroupResource = Effect.fn("EnvironmentDesign.deleteVariableGroupResource")(
  (actor: Actor, input: DeleteVariableGroupResourcePlanInput) => editResource(actor, input, "variable_group"),
);
export const updateVolumeResource = Effect.fn("EnvironmentDesign.updateVolumeResource")(
  (actor: Actor, input: UpdateVolumeResourceInput) => editResource(actor, input, "volume"),
);
export const deleteVolumeResource = Effect.fn("EnvironmentDesign.deleteVolumeResource")(
  (actor: Actor, input: DeleteVolumeResourceInput) => editResource(actor, input, "volume"),
);

export const updateEnvironmentResourceCanvasPosition = Effect.fn(
  "EnvironmentDesign.updateEnvironmentResourceCanvasPosition",
)(function* (
  actor: Actor,
  input: UpdateEnvironmentResourceCanvasPositionInput,
) {
  yield* requireEnvironmentForActorById(actor, input);
  const resource = yield* getResourceIdentity(
    input.environmentId,
    input.resourceId,
  );
  if (resource === null) {
    return yield* new NotFound({ message: "Resource not found." });
  }
  return yield* withMutationResult(
    upsertResourceCanvasPosition({
      ...input,
      resourceType: resource.implementationType,
    }),
  );
});
