import { createOptimisticAction } from "@tanstack/react-db";
import { useServerFn } from "@tanstack/react-start";
import { getRawVariablesCollection } from "#/electric/collections";
import {
  bulkUpdateServiceVariablesServerFn,
  updateServiceVariableServerFn,
} from "#/modules/environment-design/variable-functions";
import {
  buildPlainServiceVariableRecord,
  type OrganizationVariablesCollection,
} from "#/modules/environment-design/variable-collections";
import type {
  UpdateServiceVariableInput,
  VariableRecord,
} from "#/modules/environment-design/variables";
import type { RawEditorDiff } from "#/modules/environment-design/variable-raw-editor";

type UseApplyRawVariablesActionInput = {
  collection: OrganizationVariablesCollection;
  organizationSlug: string;
  environmentId: string;
  serviceId: string;
};

export type PlainVariableRecord = Omit<VariableRecord, "value"> & {
  value: Extract<VariableRecord["value"], { type: "plain" }>;
};

type SealServiceVariableActionInput = {
  organizationSlug: string;
  environmentId: string;
  serviceId: string;
};

export function buildSealServiceVariableUpdateInput({
  organizationSlug,
  environmentId,
  serviceId,
  variable,
}: SealServiceVariableActionInput & {
  variable: VariableRecord;
}): UpdateServiceVariableInput {
  if (variable.value.type !== "plain") {
    throw new Error("Only plain variables can be sealed.");
  }

  return {
    organizationSlug,
    environmentId,
    serviceId,
    variableId: variable.id,
    key: variable.key,
    description: variable.description,
    exported: variable.exported,
    value: {
      type: "sealed",
      value: variable.value.value,
    },
  };
}

export function useSealServiceVariableAction({
  organizationSlug,
  environmentId,
  serviceId,
}: SealServiceVariableActionInput) {
  const rawVariables = getRawVariablesCollection(organizationSlug);
  const updateVariable = useServerFn(updateServiceVariableServerFn);

  return async (variable: PlainVariableRecord) => {
    const receipt = await updateVariable({
      data: buildSealServiceVariableUpdateInput({
        organizationSlug,
        environmentId,
        serviceId,
        variable,
      }),
    });
    await rawVariables.utils.awaitTxId(receipt.txid);
  };
}

export function useApplyRawVariablesAction({
  collection,
  organizationSlug,
  environmentId,
  serviceId,
}: UseApplyRawVariablesActionInput) {
  const rawVariables = getRawVariablesCollection(organizationSlug);
  const bulkUpdate = useServerFn(bulkUpdateServiceVariablesServerFn);

  return createOptimisticAction<RawEditorDiff>({
    onMutate: (diff) => {
      for (const id of diff.deletes) {
        collection.delete(id);
      }
      for (const update of diff.updates) {
        collection.update(update.variableId, (draft) => {
          draft.key = update.key;
          draft.value = { type: "plain", value: update.value };
        });
      }
      for (const create of diff.creates) {
        collection.insert(
          buildPlainServiceVariableRecord({
            id: create.id,
            serviceId,
            key: create.key,
            value: create.value,
          }),
        );
      }
    },
    mutationFn: async (diff) => {
      const receipt = await bulkUpdate({
        data: {
          organizationSlug,
          environmentId,
          serviceId,
          creates: diff.creates.map((create) => ({
            id: create.id,
            key: create.key,
            description: null,
            exported: false,
            value: { type: "plain", value: create.value },
          })),
          updates: diff.updates.map((update) => ({
            variableId: update.variableId,
            key: update.key,
            value: { type: "plain", value: update.value },
          })),
          deletes: diff.deletes,
        },
      });
      await rawVariables.utils.awaitTxId(receipt.txid);
    },
  });
}
