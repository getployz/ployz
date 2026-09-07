import { eq, useLiveSuspenseQuery } from "@tanstack/react-db";
import { useServerFn } from "@tanstack/react-start";
import { getRawVariablesCollection } from "#/electric/collections";
import { parseLiveQueryRow } from "#/lib/tanstack-db";
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
  variableSelectSchema,
  type VariableRecord,
} from "#/modules/environment-design/variables";
import type { PlainVariableRecord } from "#/modules/environment-design/variable-mutation-actions";
import {
  useVariableWriter,
  useVariablesCollection,
} from "#/modules/services/services.collection";
import type { VariableGroupDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/useVariableGroupDrawerState";

export function VariableGroupVariablesTab({
  state,
}: {
  state: VariableGroupDrawerState;
}) {
  const createVariable = useServerFn(createVariableGroupVariableServerFn);
  const updateVariable = useServerFn(updateVariableGroupVariableServerFn);
  const updateVariableMetadata = useServerFn(
    updateVariableGroupVariableMetadataServerFn,
  );
  const variablesCollection = useVariablesCollection(state.organizationSlug);
  const variableWriter = useVariableWriter(state.organizationSlug);
  const { organizationSlug } = state;
  const { environmentId } = state.resource.resource;
  const variableGroupId = state.resource.variableGroup.id;

  const { data: rawVariables } = useLiveSuspenseQuery({
    query: (q) =>
      q
        .from({ variable: variablesCollection })
        .where(({ variable }) => eq(variable.variableGroupId, variableGroupId))
        .orderBy(({ variable }) => variable.key)
        .select(({ variable }) => variable),
  });
  const variables = rawVariables.map((row) =>
    parseLiveQueryRow(variableSelectSchema, row),
  );

  const valueTargets = useReferenceTargets({
    organizationSlug,
    environmentId,
    owner: { kind: "variable_group", variableGroupId },
  });

  async function handleCreateVariable(input: VariableAddInput) {
    if (input.sealed) {
      // Sealed values can't round-trip through the optimistic collection, so
      // the create goes through the server function directly.
      const receipt = await createVariable({
        data: {
          organizationSlug,
          environmentId,
          variableGroupId,
          key: input.key,
          description: null,
          exported: input.exported,
          value: { type: "sealed", value: input.value },
        },
      });
      await getRawVariablesCollection(organizationSlug).utils.awaitTxId(
        receipt.txid,
      );
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
    const receipt = await updateVariable({
      data: {
        organizationSlug,
        environmentId,
        variableGroupId,
        variableId: variable.id,
        key: variable.key,
        description: variable.description,
        exported: variable.exported,
        value: { type: "sealed", value: variable.value.value },
      },
    });
    await getRawVariablesCollection(organizationSlug).utils.awaitTxId(
      receipt.txid,
    );
  }

  async function handleUpdateMetadata(
    variable: VariableRecord,
    patch: VariableMetadataPatch,
  ) {
    const receipt = await updateVariableMetadata({
      data: {
        organizationSlug,
        environmentId,
        variableGroupId,
        variableId: variable.id,
        description: variable.description,
        exported: patch.exported ?? variable.exported,
      },
    });
    await getRawVariablesCollection(organizationSlug).utils.awaitTxId(
      receipt.txid,
    );
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
