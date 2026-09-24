import { useServerFn } from "@tanstack/react-start";
import { useEnvironmentDocumentEditor } from "#/modules/environment-design/environment-document-edit";
import {
  VariablesPanel,
  type VariableAddInput,
} from "#/components/variables/variables-panel";
import type { VariableMetadataPatch } from "#/components/variables/variable-row";
import { useReferenceTargets } from "#/components/variables/use-reference-targets";
import { insertPlainVariableGroupVariable, sealVariableIntent } from "#/modules/environment-design/variable-collections";
import {
  createVariableGroupVariableServerFn,
  updateVariableGroupVariableMetadataServerFn,
  updateVariableGroupVariableServerFn,
} from "#/modules/environment-design/variable-functions";
import {
  type VariableRecord,
} from "#/modules/environment-design/variables";
import type { PlainVariableRecord } from "#/modules/environment-design/variable-mutation-actions";
import {
  useVariableWriter,
} from "#/modules/services/services.collection";
import type { VariableGroupDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/useVariableGroupDrawerState";

export function VariableGroupVariablesTab({
  state,
}: {
  state: VariableGroupDrawerState;
}) {
  const editDocument = useEnvironmentDocumentEditor(state.organizationSlug);
  const createVariable = useServerFn(createVariableGroupVariableServerFn);
  const updateVariable = useServerFn(updateVariableGroupVariableServerFn);
  const updateVariableMetadata = useServerFn(
    updateVariableGroupVariableMetadataServerFn,
  );
  const variableWriter = useVariableWriter(state.organizationSlug);
  const { organizationSlug } = state;
  const { environmentId } = state.resource.resource;
  const variableGroupId = state.resource.variableGroup.id;

  const variables = [...state.resource.variables].sort((a, b) => a.key.localeCompare(b.key));

  const valueTargets = useReferenceTargets({
    organizationSlug,
    environmentId,
    owner: { kind: "variable_group", variableGroupId },
  });

  // Writes are optimistic: the document editor rolls back and toasts if saving fails.
  function handleCreateVariable(input: VariableAddInput) {
    if (input.sealed) {
      const id = crypto.randomUUID();
      editDocument({
        environmentId,
        apply: (intent) => {
          intent.variableGroups.find((group) => group.variableGroupId === variableGroupId)?.variables
            .push({ id, key: input.key, description: null, exported: input.exported, valueFingerprint: "pending", value: { kind: "secret", encryptedValue: null } });
        },
        save: (revision) => createVariable({ data: {
          organizationSlug, revision, environmentId, variableGroupId, id,
          key: input.key, description: null, exported: input.exported, value: { type: "sealed", value: input.value },
        } }),
        failureMessage: "Could not add this variable.",
      });
    } else {
      // Optimistic: the writer rolls back and toasts if saving fails.
      insertPlainVariableGroupVariable(variableWriter, {
        variableGroupId,
        key: input.key,
        value: input.value,
        exported: input.exported,
      });
    }
  }

  function handleSealVariable(variable: PlainVariableRecord) {
    editDocument({
      environmentId,
      apply: (intent) => {
        const entry = intent.variableGroups.find((group) => group.variableGroupId === variableGroupId)?.variables.find((entry) => entry.id === variable.id);
        if (entry) sealVariableIntent(entry);
      },
      save: (revision) => updateVariable({ data: {
        organizationSlug, revision, environmentId, variableGroupId, variableId: variable.id,
        key: variable.key, description: variable.description, exported: variable.exported,
        value: { type: "sealed", value: variable.value.value },
      } }),
      failureMessage: "Could not seal this variable.",
    });
  }

  function handleUpdateMetadata(variable: VariableRecord, patch: VariableMetadataPatch) {
    const exported = patch.exported ?? variable.exported;
    editDocument({
      environmentId,
      apply: (intent) => {
        const entry = intent.variableGroups.find((group) => group.variableGroupId === variableGroupId)?.variables.find((entry) => entry.id === variable.id);
        if (entry) entry.exported = exported;
      },
      save: (revision) => updateVariableMetadata({ data: {
        organizationSlug, revision, environmentId, variableGroupId, variableId: variable.id, description: variable.description, exported,
      } }),
      failureMessage: "Could not update this variable.",
    });
  }

  return (
    <VariablesPanel
      variables={variables}
      collection={variableWriter}
      countNoun="Variable"
      allowSealOnCreate
      defaultExported
      onCreateVariable={handleCreateVariable}
      onSealVariable={handleSealVariable}
      onUpdateMetadata={handleUpdateMetadata}
      valueTargets={valueTargets}
    />
  );
}
