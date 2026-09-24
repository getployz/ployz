import { useReducer } from "react";
import {
  initialVariableRowState,
  variableRowReducer,
} from "#/components/variables/variable-row-state";
import { VariableRowActions } from "#/components/variables/variable-row-actions";
import { VariableRowDialogs } from "#/components/variables/variable-row-dialogs";
import { VariableRowHeading } from "#/components/variables/variable-row-heading";
import { VariableRowValue } from "#/components/variables/variable-row-value";
export type { VariableMetadataPatch } from "#/components/variables/variable-row-types";
import type { VariableMetadataPatch } from "#/components/variables/variable-row-types";
import type { ReferenceTarget } from "#/modules/environment-design/variable-autocomplete";
import { referencesDeletedOwner } from "#/modules/environment-design/variable-template";
import type { VariableWriter } from "#/modules/environment-design/variable-collections";
import type { PlainVariableRecord } from "#/modules/environment-design/variable-mutation-actions";
import type { VariableRecord } from "#/modules/environment-design/variables";

export function VariableRow({
  variable,
  collection,
  valueTargets,
  onSealVariable,
  onUpdateMetadata,
  warning,
}: {
  variable: VariableRecord;
  collection: VariableWriter;
  /** Reference targets offered by the value editor's `${{ }}` autocomplete. */
  valueTargets?: ReferenceTarget[];
  onSealVariable: (variable: PlainVariableRecord) => void;
  /**
   * When provided, the row exposes export controls and badges. Owners that don't
   * model exports (e.g. plain service variables) omit this.
   */
  onUpdateMetadata?: (variable: VariableRecord, patch: VariableMetadataPatch) => void;
  warning?: string;
}) {
  const [state, dispatch] = useReducer(
    variableRowReducer,
    initialVariableRowState,
  );

  const isSealed = variable.value.type === "sealed";
  const plainValue = variable.value.type === "plain" ? variable.value.value : "";
  const showMetadata = onUpdateMetadata != null;
  const brokenRefWarning = referencesDeletedOwner(plainValue)
    ? "References a variable whose service or group was deleted — it resolves to empty at deploy."
    : null;

  // Writes are optimistic: the writer rolls back and toasts if saving fails.
  function handleSave() {
    collection.update(variable.id, (draft) => {
      draft.value = { type: "plain", value: state.editValue };
      draft.updatedAt = new Date();
    });
    dispatch({ type: "saveSucceeded" });
  }

  function handleDelete() {
    collection.delete(variable.id);
    dispatch({ type: "deleteDialogChanged", open: false });
  }

  function handleSeal() {
    if (variable.value.type !== "plain") return;
    onSealVariable({ ...variable, value: variable.value });
    dispatch({ type: "sealDialogChanged", open: false });
  }

  return (
    <div className="grid grid-cols-2 items-center gap-3 border-b py-2 last:border-b-0">
      <VariableRowHeading
        variableKey={variable.key}
        exported={variable.exported}
        showMetadata={showMetadata}
        warnings={[warning, brokenRefWarning]}
      />

      <div className="flex min-w-0 items-center gap-1.5">
      <VariableRowValue
        editing={state.editing}
        editValue={state.editValue}
        isSealed={isSealed}
        plainValue={plainValue}
        unresolvedReferences={variable.unresolvedReferences}
        revealed={state.revealed}
        valueTargets={valueTargets}
        onCancelEdit={() => dispatch({ type: "editCancelled" })}
        onChangeEditValue={(value) =>
          dispatch({ type: "editValueChanged", value })
        }
        onSave={handleSave}
        onToggleReveal={() => dispatch({ type: "revealToggled" })}
      />

      <VariableRowActions
        editing={state.editing}
        exported={variable.exported}
        isSealed={isSealed}
        plainValue={plainValue}
        showMetadata={showMetadata}
        onCancelEdit={() => dispatch({ type: "editCancelled" })}
        onOpenDeleteDialog={() =>
          dispatch({ type: "deleteDialogChanged", open: true })
        }
        onOpenEdit={(value) =>
          dispatch({ type: "editOpened", value })
        }
        onOpenSealDialog={() =>
          dispatch({ type: "sealDialogChanged", open: true })
        }
        onSave={handleSave}
        onUpdateMetadata={(patch) => onUpdateMetadata?.(variable, patch)}
      />

      </div>

      <VariableRowDialogs
        variableKey={variable.key}
        confirmSealOpen={state.confirmSealOpen}
        confirmDeleteOpen={state.confirmDeleteOpen}
        onSealOpenChange={(open) =>
          dispatch({ type: "sealDialogChanged", open })
        }
        onDeleteOpenChange={(open) =>
          dispatch({ type: "deleteDialogChanged", open })
        }
        onConfirmSeal={handleSeal}
        onConfirmDelete={handleDelete}
      />
    </div>
  );
}
