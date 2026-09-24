import { useCollectionScope } from "#/collections/use-collection-scope";
import { plainVariableIntent, variableDocumentRecord } from "./variable-document";
import { editEnvironmentDocumentAfter, useEnvironmentDocumentEditor } from "./environment-document-edit";
import { useServerFn } from "@tanstack/react-start";
import { getEnvironmentsCollection } from "#/collections/collections";
import {
  bulkUpdateServiceVariablesServerFn,
  updateServiceVariableServerFn,
} from "#/modules/environment-design/variable-functions";
import {
  buildPlainServiceVariableRecord,
  sealVariableIntent,
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
  const edit = useEnvironmentDocumentEditor(organizationSlug);
  const updateVariable = useServerFn(updateServiceVariableServerFn);

  // Optimistic: shows the variable as sealed; rolls back and toasts if sealing fails.
  return (variable: PlainVariableRecord) => { edit({
    environmentId,
    apply: (intent) => {
      const entry = intent.services.find((node) => node.id === serviceId)?.variables.find((entry) => entry.id === variable.id);
      if (entry) sealVariableIntent(entry);
    },
    save: (revision) => updateVariable({
      data: buildSealServiceVariableUpdateInput({ organizationSlug, environmentId, serviceId, variable, revision }),
    }),
    failureMessage: "Could not seal this variable.",
  }); };
}

export function useApplyRawVariablesAction({
  organizationSlug,
  environmentId,
  serviceId,
}: UseApplyRawVariablesActionInput) {
  const scope = useCollectionScope();
  const environments = getEnvironmentsCollection(organizationSlug, scope);
  const bulkUpdate = useServerFn(bulkUpdateServiceVariablesServerFn);

  const save = (diff: RawEditorDiff, revision: string) => bulkUpdate({
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
  return (diff: RawEditorDiff) => editEnvironmentDocumentAfter(organizationSlug, scope, async () => {
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
    return {
      environmentId,
      apply: (intent) => {
        const target = intent.services.find((node) => node.id === serviceId);
        if (target) target.variables = variables;
      },
      save: (revision) => save(diff, revision),
      failureMessage: "Could not apply these variables.",
    };
  }, "Could not apply these variables.");
}
