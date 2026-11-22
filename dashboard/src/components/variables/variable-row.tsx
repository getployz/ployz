import { useReducer } from "react";
import { useCommitHold } from "#/components/stageable/use-commit-hold";
import {
  initialVariableRowState,
  variableRowReducer,
} from "#/components/variables/variable-row-state";
import { VariableRowActions } from "#/components/variables/variable-row-actions";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { VariableRowHeading } from "#/components/variables/variable-row-heading";
import { VariableRowValue } from "#/components/variables/variable-row-value";
import { VariableRenameDialog } from "#/components/variables/variable-rename-dialog";
import type { Write } from "#/modules/config-store/store-write";
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
  onSetExported,
  onNeverSync,
  onRename,
  referrers,
}: {
  variable: VariableRecord;
  collection: VariableWriter;
  /** Reference targets offered by the value editor's `${{ }}` autocomplete. */
  valueTargets?: ReferenceTarget[];
  /** Seals a plain variable's value, or sets a sealed one's, in the background. */
  onSealVariable: (variable: PlainVariableRecord) => Write;
  /** Marks the variable exported, or stops exporting it. */
  onSetExported: (variable: VariableRecord, exported: boolean) => void;
  /** Marks the variable Never sync, or syncs it again. */
  onNeverSync: (variable: VariableRecord, marked: boolean) => void;
  /** With `updateReferences`, the variables using it switch to the new name too. */
  onRename: (variable: VariableRecord, to: string, updateReferences: boolean) => void;
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
  const brokenRefWarning = referencesDeletedOwner(plainValue)
    ? "References a variable whose service was deleted — it resolves to empty at deploy."
    : null;

  function handleSave() {
    // A secret's edit seals the typed value: it never becomes plain text.
    if (isSealed) {
      const typed = state.editValue;
      if (!typed) return;
      // A refusal reopens the edit with what was typed.
      onSealVariable({ ...variable, value: { type: "plain", value: typed } })
        .catch(() => dispatch({ type: "editOpened", value: typed }));
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
        warnings={[variable.broken ? `Broken reference: ${variable.broken}` : brokenRefWarning]}
      />

      <div className="flex min-w-0 items-center gap-1.5">
      <VariableRowValue
        variableKey={variable.key}
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
        variableKey={variable.key}
        editing={state.editing}
        refocus={state.refocusActions}
        exported={variable.exported}
        isSealed={isSealed}
        plainValue={plainValue}
        onCancelEdit={() => dispatch({ type: "editCancelled" })}
        onDelete={() => collection.delete(variable.id)}
        onOpenEdit={(value) =>
          dispatch({ type: "editOpened", value })
        }
        onOpenSealDialog={() =>
          dispatch({ type: "sealDialogChanged", open: true })
        }
        onSave={handleSave}
        onToggleExported={() => onSetExported(variable, !variable.exported)}
        neverSynced={variable.neverSynced ?? false}
        onToggleNeverSync={() => onNeverSync(variable, !variable.neverSynced)}
        onOpenRename={() => dispatch({ type: "renameDialogChanged", open: true })}
      />

      </div>

      {/* Sealing can't be undone here, so it asks; deleting is staged, so it doesn't. */}
      <ConfirmDialog
        open={state.confirmSealOpen}
        onOpenChange={(open) => dispatch({ type: "sealDialogChanged", open })}
        title="Seal Variable"
        description={
          <>
            Seal <strong>{variable.key}</strong>? You can't reveal or edit its value here afterwards.
          </>
        }
        actionLabel="Seal variable"
        pendingLabel="Sealing…"
        variant="destructive"
        onConfirm={handleSeal}
      />
      <VariableRenameDialog
        variableKey={variable.key}
        referrers={referrers}
        open={state.renameOpen}
        onOpenChange={(open) => dispatch({ type: "renameDialogChanged", open })}
        onRename={(to, updateReferences) => onRename(variable, to, updateReferences)}
      />
    </div>
  );
}
