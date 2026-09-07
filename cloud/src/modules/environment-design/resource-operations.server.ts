import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { withMutationReceipt } from "#/server/mutation-receipt.server";
import { slugifySegment, allocateUnique } from "#/utils/slug";
import {
  getDuplicateEnvironmentNodeNameMessage,
  isEnvironmentNodeNameTaken,
  resolveUniqueEnvironmentNodeName,
} from "./environment-node-names";
import { environmentDesignFields } from "./fields";
import {
  listEnvironmentNodeNameIdentities,
  requireEnvironmentForActorById,
} from "./authoring-repository.server";
import {
  createVariableGroupAggregate,
  createVolumeAggregate,
  deleteVariableGroupAggregate,
  getResourceIdentity,
  getVariableGroupConsumerCount,
  getVariableGroupResource,
  getVolumeResource,
  tombstoneVolume,
  updateVariableGroupName,
  updateVolumeName,
  upsertResourceCanvasPosition,
} from "./resource-repository.server";
import type {
  CreateVariableGroupResourceInput,
  CreateVolumeResourceInput,
  DeleteVariableGroupResourcePlanInput,
  DeleteVolumeResourceInput,
  UpdateEnvironmentResourceCanvasPositionInput,
  UpdateVariableGroupResourceInput,
  UpdateVolumeResourceInput,
} from "./resources";
import { Conflict, NotFound } from "#/server/public-error";

function resourceSlug(type: "variable_group" | "volume", name: string) {
  return slugifySegment(name) || (type === "volume" ? "volume" : "variable-group");
}

const createResource = Effect.fn("EnvironmentDesign.createResource")(
  function* (
    actor: Actor,
    input:
      | (CreateVariableGroupResourceInput & {
          readonly type: "variable_group";
        })
      | (CreateVolumeResourceInput & { readonly type: "volume" }),
  ) {
    const context = yield* requireEnvironmentForActorById(actor, input);
    const attemptedNames = yield* listEnvironmentNodeNameIdentities(
      input.environmentId,
    );
    return yield* withMutationReceipt(
      allocateUnique({
        tryAttempt: (attempt) =>
          Effect.gen(function* () {
            const name = resolveUniqueEnvironmentNodeName({
              name: input.name,
              nodes: attemptedNames,
              schema: environmentDesignFields.resource.name,
              maxLength: 64,
            });
            const values = {
              projectId: context.project.id,
              environmentId: input.environmentId,
              name,
              slug: resourceSlug(input.type, name),
              x: input.x,
              y: input.y,
            };
            const created =
              input.type === "variable_group"
                ? yield* createVariableGroupAggregate(values)
                : yield* createVolumeAggregate(values);
            if (created !== null) return created;
            attemptedNames.push({
              type: input.type,
              id: `attempt-${attempt}`,
              name,
            });
            return null;
          }),
        exhausted: new Conflict({
          message: `Failed to create a unique ${input.type === "volume" ? "Volume" : "Variable Group"} resource name.`,
        }),
      }),
    );
  },
);

export const createVariableGroupResource = Effect.fn(
  "EnvironmentDesign.createVariableGroupResource",
)(function* (actor: Actor, input: CreateVariableGroupResourceInput) {
  const receipt = yield* createResource(actor, {
    ...input,
    type: "variable_group",
  });
  const record = yield* getVariableGroupResource(
    input.environmentId,
    receipt.data,
  );
  if (record === null) {
    return yield* new NotFound({ message: "Variable group not found." });
  }
  return { ...receipt, data: record };
});

export const createVolumeResource = Effect.fn(
  "EnvironmentDesign.createVolumeResource",
)(function* (actor: Actor, input: CreateVolumeResourceInput) {
  const receipt = yield* createResource(actor, { ...input, type: "volume" });
  const record = yield* getVolumeResource(input.environmentId, receipt.data);
  if (record === null) {
    return yield* new NotFound({ message: "Volume not found." });
  }
  return { ...receipt, data: record };
});

const assertUniqueResourceName = Effect.fn(
  "EnvironmentDesign.assertUniqueResourceName",
)(function* (
  environmentId: string,
  name: string,
  current: { readonly type: "variable_group" | "volume"; readonly id: string },
) {
  const identities = yield* listEnvironmentNodeNameIdentities(environmentId);
  if (isEnvironmentNodeNameTaken(name, identities, current)) {
    return yield* new Conflict({
      message: getDuplicateEnvironmentNodeNameMessage(name),
    });
  }
});

export const updateVariableGroupResource = Effect.fn(
  "EnvironmentDesign.updateVariableGroupResource",
)(function* (actor: Actor, input: UpdateVariableGroupResourceInput) {
  yield* requireEnvironmentForActorById(actor, input);
  const current = yield* getVariableGroupResource(
    input.environmentId,
    input.resourceId,
  );
  if (current === null) {
    return yield* new NotFound({ message: "Variable group not found." });
  }
  yield* assertUniqueResourceName(input.environmentId, input.name, {
    type: "variable_group",
    id: current.resource.id,
  });
  const receipt = yield* withMutationReceipt(
    updateVariableGroupName(
      current.resource.id,
      current.variableGroup.id,
      input.name,
    ),
  );
  if (receipt.data === null) {
    return yield* new NotFound({ message: "Variable group not found." });
  }
  const updated = yield* getVariableGroupResource(
    input.environmentId,
    input.resourceId,
  );
  if (updated === null) {
    return yield* new NotFound({ message: "Variable group not found." });
  }
  return { ...receipt, data: updated };
});

export const deleteVariableGroupResource = Effect.fn(
  "EnvironmentDesign.deleteVariableGroupResource",
)(function* (actor: Actor, input: DeleteVariableGroupResourcePlanInput) {
  yield* requireEnvironmentForActorById(actor, input);
  const current = yield* getVariableGroupResource(
    input.environmentId,
    input.resourceId,
  );
  if (current === null) {
    return yield* new NotFound({ message: "Variable group not found." });
  }
  const consumerCount = yield* getVariableGroupConsumerCount(
    current.variableGroup.id,
  );
  if (consumerCount > 0) {
    return yield* new Conflict({
      message:
        "Variable Group has consumers and must be replaced or disconnected first.",
    });
  }
  return yield* withMutationReceipt(
    deleteVariableGroupAggregate(
      current.resource.id,
      current.variableGroup.id,
    ),
  );
});

export const updateVolumeResource = Effect.fn(
  "EnvironmentDesign.updateVolumeResource",
)(function* (actor: Actor, input: UpdateVolumeResourceInput) {
  yield* requireEnvironmentForActorById(actor, input);
  const current = yield* getVolumeResource(input.environmentId, input.resourceId);
  if (current === null) {
    return yield* new NotFound({ message: "Volume not found." });
  }
  yield* assertUniqueResourceName(input.environmentId, input.name, {
    type: "volume",
    id: current.resource.id,
  });
  const receipt = yield* withMutationReceipt(
    updateVolumeName(current.resource.id, input.name),
  );
  if (receipt.data === null) {
    return yield* new NotFound({ message: "Volume not found." });
  }
  const updated = yield* getVolumeResource(input.environmentId, input.resourceId);
  if (updated === null) {
    return yield* new NotFound({ message: "Volume not found." });
  }
  return { ...receipt, data: updated };
});

export const deleteVolumeResource = Effect.fn(
  "EnvironmentDesign.deleteVolumeResource",
)(function* (actor: Actor, input: DeleteVolumeResourceInput) {
  yield* requireEnvironmentForActorById(actor, input);
  const current = yield* getVolumeResource(input.environmentId, input.resourceId);
  if (current === null) {
    return yield* new NotFound({ message: "Volume not found." });
  }
  const receipt = yield* withMutationReceipt(tombstoneVolume(input.resourceId));
  if (receipt.data === null) {
    return yield* new NotFound({ message: "Volume not found." });
  }
  return { ...receipt, data: { resourceId: input.resourceId } };
});

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
  return yield* withMutationReceipt(
    upsertResourceCanvasPosition({
      ...input,
      resourceType: resource.implementationType,
    }),
  );
});
