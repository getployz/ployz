import { useCollectionScope } from "#/collections/use-collection-scope";
import { useEnvironmentDocument } from "#/modules/environment-design/environment-document.collection";
import { useServerFn } from "@tanstack/react-start";
import { getEnvironmentsCollection } from "#/collections/collections";
import {
  VariablesPanel,
  type VariableAddInput,
} from "#/components/variables/variables-panel";
import type { VariableMetadataPatch } from "#/components/variables/variable-row";
import { useReferenceTargets } from "#/components/variables/use-reference-targets";
import { insertPlainVariableGroupVariable } from "#/modules/environment-design/variable-collections";
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
  const collectionScope = useCollectionScope();
  const createVariable = useServerFn(createVariableGroupVariableServerFn);
  const updateVariable = useServerFn(updateVariableGroupVariableServerFn);
  const updateVariableMetadata = useServerFn(
    updateVariableGroupVariableMetadataServerFn,
  );
  const variableWriter = useVariableWriter(state.organizationSlug);
  const { organizationSlug } = state;
  const { environmentId } = state.resource.resource;
  const variableGroupId = state.resource.variableGroup.id;

  const document = useEnvironmentDocument(organizationSlug, environmentId);
  const variables = [...state.resource.variables].sort((a, b) => a.key.localeCompare(b.key));
  function revision() {
    if (!document) throw new Error("Environment is not loaded.");
    return document.revision;
  }

  const valueTargets = useReferenceTargets({
    organizationSlug,
    environmentId,
    owner: { kind: "variable_group", variableGroupId },
  });

  async function handleCreateVariable(input: VariableAddInput) {
    if (input.sealed) {
      // Sealed values can't round-trip through the optimistic collection, so
      // the create goes through the server function directly.
      const result = await createVariable({
        data: {
          organizationSlug,
          revision: revision(),
          environmentId,
          variableGroupId,
          key: input.key,
          description: null,
          exported: input.exported,
          value: { type: "sealed", value: input.value },
        },
      });
      await getEnvironmentsCollection(organizationSlug, collectionScope).writeCommitted(result.data);
    } else {
      await insertPlainVariableGroupVariable(variableWriter, {
        variableGroupId,
        key: input.key,
        value: input.value,
        exported: input.exported,
      });
    }
  }

  async function handleSealVariable(variable: PlainVariableRecord) {
    const result = await updateVariable({
      data: {
        organizationSlug,
        revision: revision(),
        environmentId,
        variableGroupId,
        variableId: variable.id,
        key: variable.key,
        description: variable.description,
        exported: variable.exported,
        value: { type: "sealed", value: variable.value.value },
      },
    });
    await getEnvironmentsCollection(organizationSlug, collectionScope).writeCommitted(result.data);
  }

  async function handleUpdateMetadata(
    variable: VariableRecord,
    patch: VariableMetadataPatch,
  ) {
    const result = await updateVariableMetadata({
      data: {
        organizationSlug,
        revision: revision(),
        environmentId,
        variableGroupId,
        variableId: variable.id,
        description: variable.description,
        exported: patch.exported ?? variable.exported,
      },
    });
    await getEnvironmentsCollection(organizationSlug, collectionScope).writeCommitted(result.data);
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
