import {
  createOptimisticAction,
  createLiveQueryCollection,
  eq,
  toArray,
  type Collection,
} from "@tanstack/react-db";
import {
  getRawServicesCollection,
  getRawVariablesCollection,
  getVariableGroupsCollection,
} from "#/electric/collections";
import { plainRowCollection } from "#/lib/tanstack-db";
import type { VariablesCollection } from "#/modules/services/services.collection";
import {
  type VariableRecord,
  type VariableValueInput,
} from "#/modules/environment-design/variables";
import {
  createServiceVariableServerFn,
  createVariableGroupVariableServerFn,
  deleteServiceVariableServerFn,
  deleteVariableGroupVariableServerFn,
  updateServiceVariableServerFn,
  updateVariableGroupVariableServerFn,
} from "#/modules/environment-design/variable-functions";
import { partsToDisplay } from "#/modules/environment-design/variable-template";

function toValueInput(value: {
  type: "plain";
  value: string;
} | {
  type: "sealed";
  hasValue: boolean;
}): VariableValueInput {
  if (value.type === "plain") {
    return { type: "plain", value: value.value };
  }
  // Sealed values cannot round-trip through the collection — the client doesn't
  // hold the plaintext. Callers must use the bulk path for sealed edits.
  throw new Error(
    "Sealed variables cannot be created or updated via the collection.",
  );
}

export type OrganizationVariablesCollectionInput = {
  organizationSlug: string;
  getServiceEnvironmentId: (serviceId: string) => string | undefined;
  getVariableGroupEnvironmentId?: (variableGroupId: string) => string | undefined;
};

export type VariableWriter = {
  insert(variable: VariableRecord): { isPersisted: { promise: Promise<unknown> } };
  update(
    variableId: string,
    updater: (draft: VariableRecord) => void,
  ): { isPersisted: { promise: Promise<unknown> } };
  delete(variableId: string): { isPersisted: { promise: Promise<unknown> } };
};

type VariableMutationContext =
  | {
      type: "service";
      serviceId: string;
      environmentId: string;
    }
  | {
      type: "variable_group";
      variableGroupId: string;
      environmentId: string;
    };

type OwnedVariableOperationInput = {
  organizationSlug: string;
  ctx: VariableMutationContext;
  variable: VariableRecord;
};

type OwnedVariableDeleteInput = {
  organizationSlug: string;
  ctx: VariableMutationContext;
  variable: Pick<VariableRecord, "id">;
};

async function createOwnedVariable(
  input: OwnedVariableOperationInput,
): Promise<{ txid: number }> {
  switch (input.ctx.type) {
    case "service":
      return await createServiceVariableServerFn({
        data: {
          ...toOwnedVariableCreateBase(input),
          serviceId: input.ctx.serviceId,
        },
      });
    case "variable_group":
      return await createVariableGroupVariableServerFn({
        data: {
          ...toOwnedVariableCreateBase(input),
          variableGroupId: input.ctx.variableGroupId,
        },
      });
  }
}

async function updateOwnedVariable(
  input: OwnedVariableOperationInput,
): Promise<{ txid: number }> {
  switch (input.ctx.type) {
    case "service":
      return await updateServiceVariableServerFn({
        data: {
          ...toOwnedVariableUpdateBase(input),
          serviceId: input.ctx.serviceId,
        },
      });
    case "variable_group":
      return await updateVariableGroupVariableServerFn({
        data: {
          ...toOwnedVariableUpdateBase(input),
          variableGroupId: input.ctx.variableGroupId,
        },
      });
  }
}

async function deleteOwnedVariable(
  input: OwnedVariableDeleteInput,
): Promise<{ txid: number }> {
  switch (input.ctx.type) {
    case "service":
      return await deleteServiceVariableServerFn({
        data: {
          ...toVariableDeleteBase(input),
          serviceId: input.ctx.serviceId,
        },
      });
    case "variable_group":
      return await deleteVariableGroupVariableServerFn({
        data: {
          ...toVariableDeleteBase(input),
          variableGroupId: input.ctx.variableGroupId,
        },
      });
  }
}

function toOwnedVariableCreateBase(input: OwnedVariableOperationInput) {
  return {
    organizationSlug: input.organizationSlug,
    environmentId: input.ctx.environmentId,
    id: input.variable.id,
    key: input.variable.key,
    description: input.variable.description,
    exported: input.variable.exported,
    value: toValueInput(input.variable.value),
  };
}

function toOwnedVariableUpdateBase(input: OwnedVariableOperationInput) {
  return {
    organizationSlug: input.organizationSlug,
    environmentId: input.ctx.environmentId,
    variableId: input.variable.id,
    key: input.variable.key,
    description: input.variable.description,
    exported: input.variable.exported,
    value: toValueInput(input.variable.value),
  };
}

function toVariableDeleteBase(input: OwnedVariableDeleteInput) {
  return {
    organizationSlug: input.organizationSlug,
    environmentId: input.ctx.environmentId,
    variableId: input.variable.id,
  };
}

export function organizationVariablesCollectionOptions(
  input: OrganizationVariablesCollectionInput,
) {
  const {
    organizationSlug,
    getServiceEnvironmentId,
    getVariableGroupEnvironmentId,
  } = input;

  function resolveMutationContext(variable: {
    serviceId: string | null;
    variableGroupId: string | null;
  }): VariableMutationContext {
    if (variable.serviceId) {
      const environmentId = getServiceEnvironmentId(variable.serviceId);
      if (!environmentId) {
        throw new Error(
          `Cannot mutate variable: service ${variable.serviceId} not found in services collection.`,
        );
      }
      return {
        type: "service",
        serviceId: variable.serviceId,
        environmentId,
      };
    }

    if (variable.variableGroupId) {
      const environmentId = getVariableGroupEnvironmentId?.(variable.variableGroupId);
      if (!environmentId) {
        throw new Error(
          `Cannot mutate variable: variable group ${variable.variableGroupId} not found in resources collection.`,
        );
      }
      return {
        type: "variable_group",
        variableGroupId: variable.variableGroupId,
        environmentId,
      };
    }

    throw new Error(
      "Cannot mutate variable: variable is not owned by a service or variable group.",
    );
  }

  const rawVariables = getRawVariablesCollection(organizationSlug);
  const rawServices = getRawServicesCollection(organizationSlug);
  const variableGroups = getVariableGroupsCollection(organizationSlug);
  const variablesWithOwners = createLiveQueryCollection({
    id: `electric:${organizationSlug}:variable-owner-relationships`,
    startSync: true,
    query: (q) => q.from({ rawVariable: rawVariables }).select(({ rawVariable }) => ({
      variable: rawVariable,
      services: toArray(
        q
          .from({ ownerService: rawServices })
          .where(({ ownerService }) => eq(ownerService.projectId, rawVariable.projectId))
          .select(({ ownerService }) => ({
            lineageId: ownerService.lineageId,
            slug: ownerService.slug,
          })),
      ),
      variableGroups: toArray(
        q
          .from({ ownerVariableGroup: variableGroups })
          .where(({ ownerVariableGroup }) =>
            eq(ownerVariableGroup.projectId, rawVariable.projectId),
          )
          .select(({ ownerVariableGroup }) => ({
            lineageId: ownerVariableGroup.lineageId,
            slug: ownerVariableGroup.slug,
          })),
      ),
    })),
  });

  const collection = createLiveQueryCollection({
      id: `electric:${organizationSlug}:variables-with-values`,
      startSync: true,
      query: (q) => q.from({ variableOwnerRelationships: variablesWithOwners }).fn.select(({ variableOwnerRelationships }) => {
        const slugByLineageId = new Map([
          ...variableOwnerRelationships.services.map((service) => [service.lineageId, service.slug] as const),
          ...variableOwnerRelationships.variableGroups.map((group) => [group.lineageId, group.slug] as const),
        ]);
        const variable = variableOwnerRelationships.variable;
        return {
          id: variable.id,
          serviceId: variable.serviceId,
          variableGroupId: variable.variableGroupId,
          configKeyId: variable.configKeyId,
          key: variable.key,
          description: variable.description,
          exported: variable.exported,
          value: variable.valueKind === "plain"
            ? {
                type: "plain" as const,
                value: partsToDisplay(
                  variable.valueParts ?? [],
                  (lineageId) => slugByLineageId.get(lineageId) ?? null,
                ),
              }
            : {
                type: "sealed" as const,
                hasValue: true as const,
                fingerprint: variable.valueFingerprint,
              },
          createdAt: variable.createdAt,
          updatedAt: variable.updatedAt,
        };
      }),
      getKey: (item) => item.id,
    });

  const readonlyCollection: Collection<
    VariableRecord,
    string | number,
    typeof collection.utils,
    never,
    VariableRecord
  > = plainRowCollection(collection);
  const insertAction = createOptimisticAction<VariableRecord>({
    onMutate: (variable) => readonlyCollection.insert(variable),
    mutationFn: async (variable) => {
      const receipt = await createOwnedVariable({
        organizationSlug,
        ctx: resolveMutationContext(variable),
        variable,
      });
      await rawVariables.utils.awaitTxId(receipt.txid);
    },
  });
  const updateAction = createOptimisticAction<VariableRecord>({
    onMutate: (variable) => {
      readonlyCollection.update(variable.id, (draft) =>
        Object.assign(draft, variable),
      );
    },
    mutationFn: async (variable) => {
      const receipt = await updateOwnedVariable({
        organizationSlug,
        ctx: resolveMutationContext(variable),
        variable,
      });
      await rawVariables.utils.awaitTxId(receipt.txid);
    },
  });
  const deleteAction = createOptimisticAction<VariableRecord>({
    onMutate: (variable) => readonlyCollection.delete(variable.id),
    mutationFn: async (variable) => {
      const receipt = await deleteOwnedVariable({
        organizationSlug,
        ctx: resolveMutationContext(variable),
        variable,
      });
      await rawVariables.utils.awaitTxId(receipt.txid);
    },
  });
  const writer: VariableWriter = {
    insert: (variable) => insertAction(variable),
    update(variableId, updater) {
      const current = readonlyCollection.get(variableId);
      if (!current) throw new Error("Variable is not loaded.");
      const modified = structuredClone(current);
      updater(modified);
      return updateAction(modified);
    },
    delete(variableId) {
      const current = readonlyCollection.get(variableId);
      if (!current) throw new Error("Variable is not loaded.");
      return deleteAction(current);
    },
  };

  return { collection: readonlyCollection, writer };
}

// Handle type for the materialized variables collection. Defined off the
// org collection bag so consumers (mutation actions, variable rows) keep the
// same `.insert/.update/.delete` surface. This is a type-only back-reference;
// it is erased at runtime, so there is no module cycle with services.collection.
export type OrganizationVariablesCollection = VariablesCollection;

export type PlainServiceVariableInsertInput = {
  id?: string;
  serviceId: string;
  key: string;
  value: string;
  description?: string | null;
  exported?: boolean;
};

export type PlainVariableGroupVariableInsertInput = {
  id?: string;
  variableGroupId: string;
  key: string;
  value: string;
  description?: string | null;
  exported?: boolean;
};

export function buildPlainServiceVariableRecord(
  input: PlainServiceVariableInsertInput,
): VariableRecord {
  const now = new Date();

  return {
    id: input.id ?? crypto.randomUUID(),
    serviceId: input.serviceId,
    variableGroupId: null,
    // The server creates the durable config key. The collection needs a
    // UUID-shaped optimistic value until the persisted row is refetched.
    configKeyId: crypto.randomUUID(),
    key: input.key,
    description: input.description ?? null,
    exported: input.exported ?? false,
    value: { type: "plain", value: input.value },
    createdAt: now,
    updatedAt: now,
  };
}

export async function insertPlainServiceVariable(
  writer: VariableWriter,
  input: PlainServiceVariableInsertInput,
) {
  const tx = writer.insert(buildPlainServiceVariableRecord(input));
  await tx.isPersisted.promise;
}

function buildPlainVariableGroupVariableRecord(
  input: PlainVariableGroupVariableInsertInput,
): VariableRecord {
  const now = new Date();

  return {
    id: input.id ?? crypto.randomUUID(),
    serviceId: null,
    variableGroupId: input.variableGroupId,
    configKeyId: crypto.randomUUID(),
    key: input.key,
    description: input.description ?? null,
    exported: input.exported ?? false,
    value: { type: "plain", value: input.value },
    createdAt: now,
    updatedAt: now,
  };
}

export async function insertPlainVariableGroupVariable(
  writer: VariableWriter,
  input: PlainVariableGroupVariableInsertInput,
) {
  const tx = writer.insert(buildPlainVariableGroupVariableRecord(input));
  await tx.isPersisted.promise;
}
