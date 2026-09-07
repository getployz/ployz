import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { withMutationReceipt } from "#/server/mutation-receipt.server";
import { Conflict, NotFound, Validation } from "#/server/public-error";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { requireEnvironmentForActorById } from "./authoring-repository.server";
import {
  attachVariableGroup,
  createOwnedVariable,
  deleteServiceVariables,
  deleteVariableRecord,
  detachVariableGroup,
  getServiceVariableOwner,
  getVariableGroupOwner,
  getVariableRow,
  loadEnvironmentRefIndex,
  updateOwnedVariable,
  updateVariableMetadata,
  validateVariableValue,
  variableValueColumnsForWrite,
} from "./variable-repository.server";
import type {
  BulkUpdateServiceVariablesInput,
  CreateServiceVariableInput,
  CreateVariableGroupVariableInput,
  UpdateServiceVariableExportInput,
  UpdateServiceVariableInput,
  UpdateVariableGroupVariableInput,
  UpdateVariableGroupVariableMetadataInput,
} from "./variables";

type EnvironmentMutationInput = {
  readonly organizationSlug: string;
  readonly environmentId: string;
};

type ServiceMutationInput = EnvironmentMutationInput & {
  readonly serviceId: string;
};

type VariableGroupMutationInput = EnvironmentMutationInput & {
  readonly variableGroupId: string;
};

const requireService = Effect.fn(
  "EnvironmentDesign.requireVariableService",
)(function* (actor: Actor, input: ServiceMutationInput) {
  const context = yield* requireEnvironmentForActorById(actor, input);
  const service = yield* getServiceVariableOwner(
    input.environmentId,
    input.serviceId,
  );
  if (service === null) {
    return yield* new NotFound({ message: "Service not found." });
  }
  return { context, service };
});

const requireVariableGroup = Effect.fn(
  "EnvironmentDesign.requireVariableGroup",
)(function* (actor: Actor, input: VariableGroupMutationInput) {
  const context = yield* requireEnvironmentForActorById(actor, input);
  const variableGroup = yield* getVariableGroupOwner(
    input.environmentId,
    input.variableGroupId,
  );
  if (variableGroup === null) {
    return yield* new NotFound({ message: "Variable group not found." });
  }
  return { context, variableGroup };
});

function assertValueTransition(input: {
  readonly currentValueKind: "plain" | "sealed";
  readonly nextValueKind: "plain" | "sealed";
}) {
  return input.currentValueKind === "sealed" && input.nextValueKind === "plain"
    ? new Validation({
        message: "Sealed variables cannot be converted back to plain variables.",
      })
    : null;
}

export const createServiceVariable = Effect.fn(
  "EnvironmentDesign.createServiceVariable",
)(function* (actor: Actor, input: CreateServiceVariableInput) {
  const encryption = yield* SecretEncryption;
  const { context, service } = yield* requireService(actor, input);
  const refIndex = yield* loadEnvironmentRefIndex(input.environmentId);
  const invalid = validateVariableValue(input.value, {
    lookupLineage: refIndex.lookupLineage,
    ownerScope: "service",
  });
  if (invalid !== null) return yield* invalid;

  return yield* withMutationReceipt(
    Effect.gen(function* () {
      const created = yield* createOwnedVariable({
        projectId: context.project.id,
        environmentId: input.environmentId,
        owner: { scope: "service_lineage", lineageId: service.lineageId },
        values: {
          id: input.id,
          serviceId: input.serviceId,
          variableGroupId: null,
          key: input.key,
          description: input.description,
          exported: input.exported,
          ...variableValueColumnsForWrite(encryption, input.value, refIndex.lookupLineage),
        },
        lookupSlug: refIndex.lookupSlug,
      });
      if (created === null) {
        return yield* Effect.die(`Failed to create variable ${input.key}.`);
      }
      return created;
    }),
  );
});

export const createVariableGroupVariable = Effect.fn(
  "EnvironmentDesign.createVariableGroupVariable",
)(function* (actor: Actor, input: CreateVariableGroupVariableInput) {
  const encryption = yield* SecretEncryption;
  const { context, variableGroup } = yield* requireVariableGroup(actor, input);
  const refIndex = yield* loadEnvironmentRefIndex(input.environmentId);
  const invalid = validateVariableValue(input.value, {
    lookupLineage: refIndex.lookupLineage,
    ownerScope: "variable_group",
  });
  if (invalid !== null) return yield* invalid;

  return yield* withMutationReceipt(
    Effect.gen(function* () {
      const created = yield* createOwnedVariable({
        projectId: context.project.id,
        environmentId: input.environmentId,
        owner: {
          scope: "variable_group_lineage",
          lineageId: variableGroup.lineageId,
        },
        values: {
          id: input.id,
          serviceId: null,
          variableGroupId: input.variableGroupId,
          key: input.key,
          description: input.description,
          exported: input.exported,
          ...variableValueColumnsForWrite(encryption, input.value, refIndex.lookupLineage),
        },
        lookupSlug: refIndex.lookupSlug,
      });
      if (created === null) {
        return yield* Effect.die(`Failed to create variable ${input.key}.`);
      }
      return created;
    }),
  );
});

const updateVariable = Effect.fn("EnvironmentDesign.updateVariable")(
  function* (
    actor: Actor,
    input:
      | (UpdateServiceVariableInput & { readonly ownerScope: "service" })
      | (UpdateVariableGroupVariableInput & {
          readonly ownerScope: "variable_group";
        }),
  ) {
    const encryption = yield* SecretEncryption;
    const context =
      input.ownerScope === "service"
        ? (yield* requireService(actor, input)).context
        : (yield* requireVariableGroup(actor, input)).context;
    const owner =
      input.ownerScope === "service"
        ? { serviceId: input.serviceId }
        : { variableGroupId: input.variableGroupId };
    const existing = yield* getVariableRow({
      variableId: input.variableId,
      ...owner,
    });
    if (existing === null) {
      return yield* new NotFound({ message: "Variable not found." });
    }
    const invalidTransition = assertValueTransition({
      currentValueKind: existing.valueKind,
      nextValueKind: input.value.type,
    });
    if (invalidTransition !== null) return yield* invalidTransition;

    const refIndex = yield* loadEnvironmentRefIndex(input.environmentId);
    const invalidValue = validateVariableValue(input.value, {
      lookupLineage: refIndex.lookupLineage,
      ownerScope: input.ownerScope,
    });
    if (invalidValue !== null) return yield* invalidValue;

    const receipt = yield* withMutationReceipt(
      updateOwnedVariable({
        projectId: context.project.id,
        environmentId: input.environmentId,
        row: existing,
        values: {
          key: input.key,
          description: input.description,
          exported: input.exported,
          ...variableValueColumnsForWrite(encryption, input.value, refIndex.lookupLineage),
        },
        lookupSlug: refIndex.lookupSlug,
      }),
    );
    if (receipt.data === null) {
      return yield* new NotFound({ message: "Variable not found." });
    }
    return { ...receipt, data: receipt.data };
  },
);

export const updateServiceVariable = Effect.fn(
  "EnvironmentDesign.updateServiceVariable",
)(function* (actor: Actor, input: UpdateServiceVariableInput) {
  return yield* updateVariable(actor, { ...input, ownerScope: "service" });
});

export const updateVariableGroupVariable = Effect.fn(
  "EnvironmentDesign.updateVariableGroupVariable",
)(function* (actor: Actor, input: UpdateVariableGroupVariableInput) {
  return yield* updateVariable(actor, {
    ...input,
    ownerScope: "variable_group",
  });
});

export const updateServiceVariableExport = Effect.fn(
  "EnvironmentDesign.updateServiceVariableExport",
)(function* (actor: Actor, input: UpdateServiceVariableExportInput) {
  yield* requireService(actor, input);
  const receipt = yield* withMutationReceipt(
    updateVariableMetadata({
      variableId: input.variableId,
      serviceId: input.serviceId,
      exported: input.exported,
    }),
  );
  if (receipt.data === null) {
    return yield* new NotFound({ message: "Variable not found." });
  }
  return { ...receipt, data: receipt.data };
});

export const updateVariableGroupVariableMetadata = Effect.fn(
  "EnvironmentDesign.updateVariableGroupVariableMetadata",
)(function* (actor: Actor, input: UpdateVariableGroupVariableMetadataInput) {
  yield* requireVariableGroup(actor, input);
  const receipt = yield* withMutationReceipt(
    updateVariableMetadata({
      variableId: input.variableId,
      variableGroupId: input.variableGroupId,
      description: input.description,
      exported: input.exported,
    }),
  );
  if (receipt.data === null) {
    return yield* new NotFound({ message: "Variable not found." });
  }
  return { ...receipt, data: receipt.data };
});

export const deleteServiceVariable = Effect.fn(
  "EnvironmentDesign.deleteServiceVariable",
)(function* (
  actor: Actor,
  input: ServiceMutationInput & { readonly variableId: string },
) {
  yield* requireService(actor, input);
  const receipt = yield* withMutationReceipt(
    deleteVariableRecord({
      variableId: input.variableId,
      serviceId: input.serviceId,
    }),
  );
  if (receipt.data === null) {
    return yield* new NotFound({ message: "Variable not found." });
  }
  return receipt;
});

export const deleteVariableGroupVariable = Effect.fn(
  "EnvironmentDesign.deleteVariableGroupVariable",
)(function* (
  actor: Actor,
  input: VariableGroupMutationInput & { readonly variableId: string },
) {
  yield* requireVariableGroup(actor, input);
  const receipt = yield* withMutationReceipt(
    deleteVariableRecord({
      variableId: input.variableId,
      variableGroupId: input.variableGroupId,
    }),
  );
  if (receipt.data === null) {
    return yield* new NotFound({ message: "Variable not found." });
  }
  return receipt;
});

function assertUniqueBulkKeys(
  creates: BulkUpdateServiceVariablesInput["creates"],
  updates: BulkUpdateServiceVariablesInput["updates"],
) {
  const seen = new Set<string>();
  for (const value of [...creates, ...updates]) {
    if (seen.has(value.key)) {
      return new Conflict({
        message: `Duplicate variable key: ${value.key}`,
      });
    }
    seen.add(value.key);
  }
  return null;
}

export const bulkUpdateServiceVariables = Effect.fn(
  "EnvironmentDesign.bulkUpdateServiceVariables",
)(function* (actor: Actor, input: BulkUpdateServiceVariablesInput) {
  const encryption = yield* SecretEncryption;
  const { context, service } = yield* requireService(actor, input);
  const duplicate = assertUniqueBulkKeys(input.creates, input.updates);
  if (duplicate !== null) return yield* duplicate;

  const refIndex = yield* loadEnvironmentRefIndex(input.environmentId);
  for (const entry of [...input.creates, ...input.updates]) {
    const invalid = validateVariableValue(entry.value, {
      lookupLineage: refIndex.lookupLineage,
      ownerScope: "service",
    });
    if (invalid !== null) return yield* invalid;
  }

  return yield* withMutationReceipt(
    Effect.gen(function* () {
      yield* deleteServiceVariables(input.serviceId, input.deletes);

      for (const update of input.updates) {
        const existing = yield* getVariableRow({
          variableId: update.variableId,
          serviceId: input.serviceId,
        });
        if (existing === null) {
          return yield* new Conflict({
            message: `Variable ${update.variableId} changed before update. Refresh and try again.`,
          });
        }
        const invalidTransition = assertValueTransition({
          currentValueKind: existing.valueKind,
          nextValueKind: update.value.type,
        });
        if (invalidTransition !== null) return yield* invalidTransition;
        const updated = yield* updateOwnedVariable({
          projectId: context.project.id,
          environmentId: input.environmentId,
          row: existing,
          values: {
            key: update.key,
            description: existing.description,
            exported: existing.exported,
            ...variableValueColumnsForWrite(
              encryption,
              update.value,
              refIndex.lookupLineage,
            ),
          },
          lookupSlug: refIndex.lookupSlug,
        });
        if (updated === null) {
          return yield* new Conflict({
            message: `Could not update variable ${update.key}. Refresh and try again.`,
          });
        }
      }

      for (const create of input.creates) {
        const created = yield* createOwnedVariable({
          projectId: context.project.id,
          environmentId: input.environmentId,
          owner: { scope: "service_lineage", lineageId: service.lineageId },
          values: {
            id: create.id,
            serviceId: input.serviceId,
            variableGroupId: null,
            key: create.key,
            description: create.description,
            exported: create.exported,
            ...variableValueColumnsForWrite(
              encryption,
              create.value,
              refIndex.lookupLineage,
            ),
          },
          lookupSlug: refIndex.lookupSlug,
        });
        if (created === null) {
          return yield* new Conflict({
            message: `Could not create variable ${create.key}. Refresh and try again.`,
          });
        }
      }
      return null;
    }),
  );
});

export const attachServiceVariableGroup = Effect.fn(
  "EnvironmentDesign.attachServiceVariableGroup",
)(function* (
  actor: Actor,
  input: ServiceMutationInput & { readonly variableGroupId: string },
) {
  yield* requireService(actor, input);
  const variableGroup = yield* getVariableGroupOwner(
    input.environmentId,
    input.variableGroupId,
  );
  if (variableGroup === null) {
    return yield* new NotFound({ message: "Variable group not found." });
  }
  return yield* withMutationReceipt(
    attachVariableGroup(input.serviceId, variableGroup),
  );
});

export const detachServiceVariableGroup = Effect.fn(
  "EnvironmentDesign.detachServiceVariableGroup",
)(function* (
  actor: Actor,
  input: ServiceMutationInput & { readonly variableGroupId: string },
) {
  yield* requireService(actor, input);
  return yield* withMutationReceipt(
    detachVariableGroup(input.serviceId, input.variableGroupId),
  );
});
