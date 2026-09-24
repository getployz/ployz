import type { CollectionScope } from "#/collections/scope";
import { editEnvironmentDocumentAfter } from "./environment-document-edit";
import type { SavedVariableIntent } from "./saved-intent";
import { getEnvironmentsCollection } from "#/collections/collections";
import type { VariableRecord } from "./variables";
import { plainVariableIntent, variableDocumentRecord } from "./variable-document";
import {
  createServiceVariableServerFn, deleteServiceVariableServerFn, updateServiceVariableServerFn,
} from "./variable-functions";

export type VariableWriter = {
  insert(variable: VariableRecord): { isPersisted: { promise: Promise<unknown> } };
  update(variableId: string, updater: (draft: VariableRecord) => void): { isPersisted: { promise: Promise<unknown> } };
  delete(variableId: string): { isPersisted: { promise: Promise<unknown> } };
};
export type OrganizationVariablesCollection = VariableWriter;

export function createVariableWriter(organizationSlug: string, scope: CollectionScope): VariableWriter {
  const environments = getEnvironmentsCollection(organizationSlug, scope);

  function currentVariable(id: string) {
    for (const document of environments.values()) {
      for (const service of document.intent.services) {
        const variable = service.variables.find((variable) => variable.id === id);
        if (variable) return variableDocumentRecord(variable, service.id, document.intent, document.updatedAt);
      }
    }
    throw new Error("Variable is not loaded.");
  }

  function save(kind: "insert" | "update" | "delete", variable: VariableRecord, environmentId: string, revision: string) {
    const scope = { organizationSlug, environmentId, revision };
    const value = variable.value;
    if (kind !== "delete" && value.type !== "plain") throw new Error("Use the sealed-variable action to edit a secret.");
    const data = { ...scope, key: variable.key, description: variable.description, exported: variable.exported,
      value: { type: "plain" as const, value: value.type === "plain" ? value.value : "" } };
    return kind === "delete" ? deleteServiceVariableServerFn({ data: { ...scope, serviceId: variable.serviceId, variableId: variable.id } })
      : kind === "insert" ? createServiceVariableServerFn({ data: { ...data, serviceId: variable.serviceId, id: variable.id } })
      : updateServiceVariableServerFn({ data: { ...data, serviceId: variable.serviceId, variableId: variable.id } });
  }

  function edit(kind: "insert" | "update" | "delete", variable: VariableRecord) {
    return editEnvironmentDocumentAfter(organizationSlug, scope, async () => {
      const document = Array.from(environments.values()).find((document) => document.intent.services
        .some((service) => service.id === variable.serviceId));
      if (!document) throw new Error("Variable owner is not loaded.");
      let authored: SavedVariableIntent | null = null;
      if (kind !== "delete") {
        if (variable.value.type !== "plain") throw new Error("Use the sealed-variable action to edit a secret.");
        authored = await plainVariableIntent(variable, document.intent);
      }
      return {
        environmentId: document.id,
        apply: (intent) => {
          const owner = intent.services.find((service) => service.id === variable.serviceId);
          if (!owner) return;
          const index = owner.variables.findIndex((entry) => entry.id === variable.id);
          if (kind === "delete") {
            if (index !== -1) owner.variables.splice(index, 1);
          } else if (authored) {
            if (index === -1) owner.variables.push(authored);
            else owner.variables[index] = authored;
          }
        },
        save: (revision) => save(kind, variable, document.id, revision),
        failureMessage: kind === "delete" ? "Could not delete this variable." : "Could not save this variable.",
      };
    }, "Could not save this variable.");
  }
  return { insert: (variable) => edit("insert", variable),
    update(id, updater) { const variable = currentVariable(id); updater(variable); return edit("update", variable); },
    delete: (id) => edit("delete", currentVariable(id)) };
}

/** Shows a variable as sealed until the server returns its encrypted value and fingerprint. */
export function sealVariableIntent(variable: SavedVariableIntent) {
  variable.value = { kind: "secret", encryptedValue: null };
}

export type PlainServiceVariableInsertInput = {
  id?: string;
  serviceId: string;
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
    key: input.key,
    description: input.description ?? null,
    exported: input.exported ?? false,
    value: { type: "plain", value: input.value },
    createdAt: now,
    updatedAt: now,
  };
}

export function insertPlainServiceVariable(
  writer: VariableWriter,
  input: PlainServiceVariableInsertInput,
) {
  return writer.insert(buildPlainServiceVariableRecord(input));
}
