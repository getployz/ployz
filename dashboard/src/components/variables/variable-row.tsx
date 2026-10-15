import { useReducer } from "react";
import { useCommitHold } from "#/components/stageable/use-commit-hold";
import {
  initialVariableRowState,
  variableRowReducer,
} from "#/components/variables/variable-row-state";
import { VariableRowActions } from "#/components/variables/variable-row-actions";
import { VariableRowDialogs } from "#/components/variables/variable-row-dialogs";
import { VariableRowHeading } from "#/components/variables/variable-row-heading";
import { VariableRowValue } from "#/components/variables/variable-row-value";
import { VariableRenameDialog } from "#/components/variables/variable-rename-dialog";
export type { VariableMetadataPatch } from "#/components/variables/variable-row-types";
import type { VariableMetadataPatch } from "#/components/variables/variable-row-types";
import type { ReferenceTarget } from "#/modules/variables/variable-autocomplete";
import { referencesDeletedOwner } from "#/modules/variables/variable-template";
import type { VariableWriter } from "#/modules/variables/variables";
import type { PlainVariableRecord } from "#/modules/variables/variables";
import type { VariableRecord } from "#/modules/variables/variables";

export function VariableRow({
  variable,
  collection,
  valueTargets,
  onSealVariable,
  onUpdateMetadata,
  onNeverSync,
  onRename,
  referrers,
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
  /** When provided, the menu marks the variable Never sync, or syncs it again. */
  onNeverSync?: (variable: VariableRecord, marked: boolean) => void;
  /** When provided, the menu renames the variable; with `updateReferences`, the variables using it switch too. */
  onRename?: (variable: VariableRecord, to: string, updateReferences: boolean) => void;
  /** The variables that reference this one, as `SERVICE.KEY`, which a rename switches to the new name. */
  referrers?: readonly string[];
}) {
  const [state, dispatch] = useReducer(
    variableRowReducer,
    initialVariableRowState,
  );

  const isSealed = variable.value.type === "sealed";
  const notSet = variable.value.type === "sealed" && variable.value.notSet === true;
  const [plainValue, hold] = useCommitHold(variable.value.type === "plain" ? variable.value.value : "");
  const showMetadata = onUpdateMetadata != null;
  const brokenRefWarning = referencesDeletedOwner(plainValue)
    ? "References a variable whose service was deleted — it resolves to empty at deploy."
    : null;

  function handleSave() {
    // A secret's edit seals the typed value: it never becomes plain text.
    if (isSealed) {
      if (!state.editValue) return;
      onSealVariable({ ...variable, value: { type: "plain", value: state.editValue } });
      dispatch({ type: "saveSucceeded" });
      return;
    }
    hold(state.editValue, collection.update(variable.id, (draft) => {
      draft.value = { type: "plain", value: state.editValue };
    }));
    dispatch({ type: "saveSucceeded" });
  }

  function handleSeal() {
    if (variable.value.type !== "plain") return;
    onSealVariable({ ...variable, value: variable.value });
    dispatch({ type: "sealDialogChanged", open: false });
  }

  return (
    <div data-changed={variable.changed || undefined}
      className="grid grid-cols-2 items-center gap-3 border-b py-2 last:border-b-0 data-[changed=true]:bg-changed-soft">
      <VariableRowHeading
        variableKey={variable.key}
        exported={variable.exported}
        neverSynced={variable.neverSynced ?? false}
        showMetadata={showMetadata}
        warnings={[brokenRefWarning]}
      />

      <div className="flex min-w-0 items-center gap-1.5">
      <VariableRowValue
        editing={state.editing}
        editValue={state.editValue}
        isSealed={isSealed}
        notSet={notSet}
        plainValue={plainValue}
        unresolvedReferences={variable.unresolvedReferences}
        revealed={state.revealed}
        valueTargets={valueTargets}
        onCancelEdit={() => dispatch({ type: "editCancelled" })}
        onChangeEditValue={(value) =>
          dispatch({ type: "editValueChanged", value })
        }
        onSave={handleSave}
        onSetValue={() => dispatch({ type: "editOpened", value: "" })}
        onToggleReveal={() => dispatch({ type: "revealToggled" })}
      />

      <VariableRowActions
        editing={state.editing}
        exported={variable.exported}
        isSealed={isSealed}
        plainValue={plainValue}
        showMetadata={showMetadata}
        onCancelEdit={() => dispatch({ type: "editCancelled" })}
        onDelete={() => collection.delete(variable.id)}
        onOpenEdit={(value) =>
          dispatch({ type: "editOpened", value })
        }
        onOpenSealDialog={() =>
          dispatch({ type: "sealDialogChanged", open: true })
        }
        onSave={handleSave}
        onUpdateMetadata={(patch) => onUpdateMetadata?.(variable, patch)}
        neverSynced={onNeverSync ? variable.neverSynced ?? false : null}
        onToggleNeverSync={() => onNeverSync?.(variable, !variable.neverSynced)}
        onOpenRename={onRename ? () => dispatch({ type: "renameDialogChanged", open: true }) : null}
      />

      </div>

      <VariableRowDialogs
        variableKey={variable.key}
        confirmSealOpen={state.confirmSealOpen}
        onSealOpenChange={(open) =>
          dispatch({ type: "sealDialogChanged", open })
        }
        onConfirmSeal={handleSeal}
      />
      {onRename ? (
        <VariableRenameDialog
          variableKey={variable.key}
          referrers={referrers}
          open={state.renameOpen}
          onOpenChange={(open) => dispatch({ type: "renameDialogChanged", open })}
          onRename={(to, updateReferences) => onRename(variable, to, updateReferences)}
        />
      ) : null}
    </div>
  );
}
