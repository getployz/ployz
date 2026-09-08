import { createOptimisticAction } from "@tanstack/react-db";
import { type SavedEnvironmentIntent, type SavedVariableIntent } from "@ployz/sdk/config";
import { getEnvironmentsCollection } from "#/electric/collections";
import type { VariableRecord } from "./variables";
import { plainVariableIntent, variableDocumentRecord } from "./variable-document";
import {
  createServiceVariableServerFn, createVariableGroupVariableServerFn,
  deleteServiceVariableServerFn, deleteVariableGroupVariableServerFn,
  updateServiceVariableServerFn, updateVariableGroupVariableServerFn,
} from "./variable-functions";

export type VariableWriter = {
  insert(variable: VariableRecord): { isPersisted: { promise: Promise<unknown> } };
  update(variableId: string, updater: (draft: VariableRecord) => void): { isPersisted: { promise: Promise<unknown> } };
  delete(variableId: string): { isPersisted: { promise: Promise<unknown> } };
};
export type OrganizationVariablesCollection = VariableWriter;

function variableOwners(intent: SavedEnvironmentIntent) {
  return [
    ...intent.services.map((node) => ({ serviceId: node.id, variableGroupId: null, variables: node.variables })),
    ...intent.variableGroups.map((node) => ({ serviceId: null, variableGroupId: node.variableGroupId, variables: node.variables })),
  ];
}

export function createVariableWriter(organizationSlug: string): VariableWriter {
  const environments = getEnvironmentsCollection(organizationSlug);
  type Edit = { environmentId: string; revision: string; kind: "insert" | "update" | "delete";
    variable: VariableRecord; authored: SavedVariableIntent | null };
  const persist = createOptimisticAction<Edit>({
    onMutate: ({ environmentId, kind, variable, authored }) => {
      environments.update(environmentId, (draft) => {
        const owner = variableOwners(draft.intent).find((owner) => owner.serviceId === variable.serviceId && owner.variableGroupId === variable.variableGroupId);
        if (!owner) throw new Error("Variable owner is not loaded.");
        const index = owner.variables.findIndex((entry) => entry.id === variable.id);
        if (kind === "delete") {
          if (index !== -1) owner.variables.splice(index, 1);
        } else if (authored) {
          if (index === -1) owner.variables.push(authored);
          else owner.variables[index] = authored;
        }
      });
    },
    mutationFn: async ({ environmentId, revision, kind, variable }) => {
      const scope = { organizationSlug, environmentId, revision };
      const value = variable.value;
      if (kind !== "delete" && value.type !== "plain") throw new Error("Use the sealed-variable action to edit a secret.");
      const data = { ...scope, key: variable.key, description: variable.description, exported: variable.exported,
        value: { type: "plain" as const, value: value.type === "plain" ? value.value : "" } };
      const receipt = variable.serviceId
        ? kind === "delete" ? await deleteServiceVariableServerFn({ data: { ...scope, serviceId: variable.serviceId, variableId: variable.id } })
          : kind === "insert" ? await createServiceVariableServerFn({ data: { ...data, serviceId: variable.serviceId, id: variable.id } })
          : await updateServiceVariableServerFn({ data: { ...data, serviceId: variable.serviceId, variableId: variable.id } })
        : variable.variableGroupId
          ? kind === "delete" ? await deleteVariableGroupVariableServerFn({ data: { ...scope, variableGroupId: variable.variableGroupId, variableId: variable.id } })
            : kind === "insert" ? await createVariableGroupVariableServerFn({ data: { ...data, variableGroupId: variable.variableGroupId, id: variable.id } })
            : await updateVariableGroupVariableServerFn({ data: { ...data, variableGroupId: variable.variableGroupId, variableId: variable.id } })
          : (() => { throw new Error("Variable has no owner."); })();
      await environments.utils.awaitTxId(receipt.txid);
    },
  });

  function currentVariable(id: string) {
    for (const document of environments.values()) {
      for (const owner of variableOwners(document.intent)) {
        const variable = owner.variables.find((variable) => variable.id === id);
        if (variable) return variableDocumentRecord(variable, owner, document.intent, document.updatedAt);
      }
    }
    throw new Error("Variable is not loaded.");
  }

  function edit(kind: Edit["kind"], variable: VariableRecord) {
    const promise = (async () => {
      const document = Array.from(environments.values()).find((document) => variableOwners(document.intent)
        .some((owner) => owner.serviceId === variable.serviceId && owner.variableGroupId === variable.variableGroupId));
      if (!document) throw new Error("Variable owner is not loaded.");
      let authored: SavedVariableIntent | null = null;
      if (kind !== "delete") {
        if (variable.value.type !== "plain") throw new Error("Use the sealed-variable action to edit a secret.");
        authored = await plainVariableIntent(variable, document.intent);
      }
      await persist({ environmentId: document.id, revision: document.revision, kind, variable, authored }).isPersisted.promise;
    })();
    return { isPersisted: { promise } };
  }
  return { insert: (variable) => edit("insert", variable),
    update(id, updater) { const variable = currentVariable(id); updater(variable); return edit("update", variable); },
    delete: (id) => edit("delete", currentVariable(id)) };
}

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
