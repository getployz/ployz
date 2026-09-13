import { useCollectionScope } from "#/collections/use-collection-scope";
import { plainVariableIntent, variableDocumentRecord } from "./variable-document";
import type { SavedVariableIntent } from "./saved-intent";
import { createOptimisticAction } from "@tanstack/react-db";
import { useServerFn } from "@tanstack/react-start";
import { getEnvironmentsCollection } from "#/collections/collections";
import {
  bulkUpdateServiceVariablesServerFn,
  updateServiceVariableServerFn,
} from "#/modules/environment-design/variable-functions";
import {
  buildPlainServiceVariableRecord,
} from "#/modules/environment-design/variable-collections";
import type {
  UpdateServiceVariableInput,
  VariableRecord,
} from "#/modules/environment-design/variables";
import type { RawEditorDiff } from "#/modules/environment-design/variable-raw-editor";

type UseApplyRawVariablesActionInput = {
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
  variable, revision,
}: SealServiceVariableActionInput & {
  variable: VariableRecord; revision: string;
}): UpdateServiceVariableInput {
  if (variable.value.type !== "plain") {
    throw new Error("Only plain variables can be sealed.");
  }

  return {
    organizationSlug,
    environmentId,
    serviceId,
    revision,
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
  const collectionScope = useCollectionScope();
  const environments = getEnvironmentsCollection(organizationSlug, collectionScope);
  const updateVariable = useServerFn(updateServiceVariableServerFn);

  return async (variable: PlainVariableRecord) => {
    const document = environments.get(environmentId);
    if (!document) throw new Error("Environment is not loaded.");
    const result = await updateVariable({
      data: buildSealServiceVariableUpdateInput({
        organizationSlug,
        environmentId,
        serviceId,
        variable, revision: document.revision,
      }),
    });
    await environments.writeCommitted(result.data);
  };
}

export function useApplyRawVariablesAction({
  organizationSlug,
  environmentId,
  serviceId,
}: UseApplyRawVariablesActionInput) {
  const collectionScope = useCollectionScope();
  const environments = getEnvironmentsCollection(organizationSlug, collectionScope);
  const bulkUpdate = useServerFn(bulkUpdateServiceVariablesServerFn);

  const persist = createOptimisticAction<{ diff: RawEditorDiff; revision: string; variables: SavedVariableIntent[] }>({
    onMutate: ({ variables }) => {
      environments.update(environmentId, (draft) => {
        const node = draft.intent.services.find((node) => node.id === serviceId);
        if (!node) throw new Error("Service is not loaded.");
        node.variables = variables;
      });
    },
    mutationFn: async ({ diff, revision }) => {
      const result = await bulkUpdate({
        data: {
          organizationSlug, revision,
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
      await environments.writeCommitted(result.data);
    },
  });
  return (diff: RawEditorDiff) => ({ isPersisted: { promise: (async () => {
    const document = environments.get(environmentId);
    const node = document?.intent.services.find((node) => node.id === serviceId);
    if (!document || !node) throw new Error("Service is not loaded.");
    const remaining = node.variables.filter((variable) => !diff.deletes.includes(variable.id));
    const variables = await Promise.all(remaining.map(async (variable) => {
      const update = diff.updates.find((update) => update.variableId === variable.id);
      return update ? plainVariableIntent({ ...variableDocumentRecord(variable, { serviceId, variableGroupId: null }, document.intent, document.updatedAt),
        key: update.key, value: { type: "plain", value: update.value } }, document.intent) : variable;
    }));
    for (const create of diff.creates) variables.push(await plainVariableIntent(buildPlainServiceVariableRecord({ ...create, serviceId }), document.intent));
    await persist({ diff, revision: document.revision, variables }).isPersisted.promise;
  })() } });
}
