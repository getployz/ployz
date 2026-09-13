import { useReducer } from "react";
import { toast } from "sonner";
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
  onSealVariable: (variable: PlainVariableRecord) => Promise<void>;
  /**
   * When provided, the row exposes export controls and badges. Owners that don't
   * model exports (e.g. plain service variables) omit this.
   */
  onUpdateMetadata?: (
    variable: VariableRecord,
    patch: VariableMetadataPatch,
  ) => Promise<void>;
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

  async function handleSave() {
    dispatch({ type: "saveStarted" });
    const tx = collection.update(variable.id, (draft) => {
      draft.value = { type: "plain", value: state.editValue };
      draft.updatedAt = new Date();
    });
    try {
      await tx.isPersisted.promise;
      dispatch({ type: "saveSucceeded" });
    } catch (error) {
      dispatch({ type: "saveFailed" });
      toast.error(
        error instanceof Error ? error.message : "Could not update variable.",
      );
    }
  }

  async function handleDelete() {
    const tx = collection.delete(variable.id);
    try {
      await tx.isPersisted.promise;
      dispatch({ type: "deleteDialogChanged", open: false });
    } catch (error) {
      toast.error(
        error instanceof Error ? error.message : "Could not delete variable.",
      );
      throw error;
    }
  }

  async function handleSeal() {
    if (variable.value.type !== "plain") return;
    try {
      await onSealVariable({
        ...variable,
        value: variable.value,
      });
      dispatch({ type: "sealDialogChanged", open: false });
    } catch (error) {
      toast.error(
        error instanceof Error ? error.message : "Could not seal variable.",
      );
    }
  }

  async function handleUpdateMetadata(patch: VariableMetadataPatch) {
    if (!onUpdateMetadata) return;
    try {
      await onUpdateMetadata(variable, patch);
    } catch (error) {
      toast.error(
        error instanceof Error ? error.message : "Could not update variable.",
      );
    }
  }

  async function handleCopy() {
    const clipboard = globalThis.navigator?.clipboard;
    if (!clipboard) {
      toast.error("Couldn't access the clipboard");
      return;
    }
    if (isSealed) {
      toast.error("Sealed values can't be copied");
      return;
    }
    await clipboard.writeText(plainValue);
    toast.info("Copied to clipboard");
  }

  return (
    <div className="grid grid-cols-[minmax(8rem,14rem)_1fr_auto] items-center gap-3 border-b py-2 last:border-b-0">
      <VariableRowHeading
        variableKey={variable.key}
        exported={variable.exported}
        showMetadata={showMetadata}
        warnings={[warning, brokenRefWarning]}
      />

      <VariableRowValue
        editing={state.editing}
        editValue={state.editValue}
        isSaving={state.isSaving}
        isSealed={isSealed}
        plainValue={plainValue}
        revealed={state.revealed}
        valueTargets={valueTargets}
        onCancelEdit={() => dispatch({ type: "editCancelled" })}
        onChangeEditValue={(value) =>
          dispatch({ type: "editValueChanged", value })
        }
        onCopy={() => void handleCopy()}
        onSave={() => void handleSave()}
        onToggleReveal={() => dispatch({ type: "revealToggled" })}
      />

      <VariableRowActions
        editing={state.editing}
        exported={variable.exported}
        isSaving={state.isSaving}
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
        onSave={() => void handleSave()}
        onUpdateMetadata={(patch) => void handleUpdateMetadata(patch)}
      />

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
