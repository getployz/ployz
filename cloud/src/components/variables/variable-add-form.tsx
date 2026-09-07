import { useReducer } from "react";
import { CheckIcon } from "lucide-react";
import { toast } from "sonner";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { Button } from "#/components/ui/button";
import { Checkbox } from "#/components/ui/checkbox";
import { Field, FieldGroup, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { Spinner } from "#/components/ui/spinner";
import { VariableValueInput } from "#/components/variables/VariableValueInput";
import type { ReferenceTarget } from "#/modules/environment-design/variable-autocomplete";
import { getSealedVariableCollisionMessage } from "#/modules/environment-design/variable-raw-editor";
import type { VariableWriter } from "#/modules/environment-design/variable-collections";
import type { VariableRecord } from "#/modules/environment-design/variables";
import type { VariableAddInput } from "#/components/variables/variables-panel";

type VariableAddFormDefaults = {
  allowSealOnCreate: boolean;
  defaultExported: boolean;
};

type VariableAddFormState = {
  key: string;
  value: string;
  sealed: boolean;
  exported: boolean;
  isSubmitting: boolean;
  overwriteCandidate: {
    id: string;
    value: string;
  } | null;
};

type VariableAddFormAction =
  | { type: "keyChanged"; value: string }
  | { type: "valueChanged"; value: string }
  | { type: "sealedChanged"; checked: boolean }
  | { type: "exportedChanged"; checked: boolean }
  | { type: "submitStarted" }
  | { type: "submitFailed" }
  | { type: "overwriteRequested"; id: string; value: string }
  | { type: "overwriteCleared" }
  | { type: "reset"; defaults: VariableAddFormDefaults };

function createVariableAddFormState({
  allowSealOnCreate,
  defaultExported,
}: VariableAddFormDefaults): VariableAddFormState {
  return {
    key: "",
    value: "",
    sealed: allowSealOnCreate,
    exported: defaultExported,
    isSubmitting: false,
    overwriteCandidate: null,
  };
}

function variableAddFormReducer(
  state: VariableAddFormState,
  action: VariableAddFormAction,
): VariableAddFormState {
  switch (action.type) {
    case "keyChanged":
      return { ...state, key: action.value };
    case "valueChanged":
      return { ...state, value: action.value };
    case "sealedChanged":
      return { ...state, sealed: action.checked };
    case "exportedChanged":
      return { ...state, exported: action.checked };
    case "submitStarted":
      return { ...state, isSubmitting: true };
    case "submitFailed":
      return { ...state, isSubmitting: false };
    case "overwriteRequested":
      return {
        ...state,
        overwriteCandidate: { id: action.id, value: action.value },
      };
    case "overwriteCleared":
      return { ...state, overwriteCandidate: null };
    case "reset":
      return createVariableAddFormState(action.defaults);
  }
}

export function VariableAddForm({
  variables,
  collection,
  onCreateVariable,
  onCancel,
  allowSealOnCreate,
  defaultExported,
  supportsExport,
  valueTargets,
}: {
  variables: VariableRecord[];
  collection: VariableWriter;
  onCreateVariable: (input: VariableAddInput) => Promise<void>;
  onCancel: () => void;
  allowSealOnCreate: boolean;
  defaultExported: boolean;
  supportsExport: boolean;
  valueTargets?: ReferenceTarget[];
}) {
  const defaults = { allowSealOnCreate, defaultExported };
  const [state, dispatch] = useReducer(
    variableAddFormReducer,
    defaults,
    createVariableAddFormState,
  );

  function closeForm() {
    dispatch({ type: "reset", defaults });
    onCancel();
  }

  async function handleAdd() {
    const key = state.key.trim().toUpperCase();
    if (!key) return;

    const existing = variables.find((variable) => variable.key === key);
    if (existing) {
      if (existing.value.type === "sealed") {
        toast.error(getSealedVariableCollisionMessage(existing.key));
        return;
      }
      dispatch({
        type: "overwriteRequested",
        id: existing.id,
        value: state.value,
      });
      return;
    }

    dispatch({ type: "submitStarted" });
    try {
      await onCreateVariable({
        key,
        value: state.value,
        sealed: state.sealed,
        exported: state.exported,
      });
      closeForm();
    } catch (error) {
      dispatch({ type: "submitFailed" });
      toast.error(
        error instanceof Error
          ? error.message
          : "The variable couldn’t be added. Check the name and try again.",
      );
    }
  }

  async function handleConfirmOverwrite() {
    if (!state.overwriteCandidate) return;
    const { id, value } = state.overwriteCandidate;
    try {
      const tx = collection.update(id, (draft) => {
        draft.value = { type: "plain", value };
        draft.updatedAt = new Date();
      });
      await tx.isPersisted.promise;
      closeForm();
    } catch (error) {
      toast.error(
        error instanceof Error
          ? error.message
          : "The variable couldn’t be overwritten. Try again.",
      );
    }
  }

  return (
    <>
      <FieldGroup>
        <Field>
          <FieldLabel htmlFor="variable-key">Key</FieldLabel>
          <Input
            id="variable-key"
            autoFocus
            placeholder="VARIABLE_NAME"
            value={state.key}
            onChange={(event) =>
              dispatch({ type: "keyChanged", value: event.target.value })
            }
            className="font-mono text-xs"
          />
        </Field>
        <Field>
          <FieldLabel htmlFor="variable-value">Value</FieldLabel>
          {state.sealed ? (
            <Input
              id="variable-value"
              type="password"
              placeholder="VALUE"
              value={state.value}
              onChange={(event) =>
                dispatch({ type: "valueChanged", value: event.target.value })
              }
              className="font-mono text-xs"
            />
          ) : (
            <VariableValueInput
              id="variable-value"
              placeholder="VALUE"
              value={state.value}
              onValueChange={(value) =>
                dispatch({ type: "valueChanged", value })
              }
              targets={valueTargets ?? []}
              className="font-mono text-xs"
            />
          )}
        </Field>
        {allowSealOnCreate || supportsExport ? (
          <FieldGroup className="gap-3">
            {allowSealOnCreate ? (
              <Field orientation="horizontal">
                <Checkbox
                  id="new-variable-sealed"
                  checked={state.sealed}
                  onCheckedChange={(checked) =>
                    dispatch({
                      type: "sealedChanged",
                      checked: Boolean(checked),
                    })
                  }
                />
                <FieldLabel htmlFor="new-variable-sealed" className="font-normal">
                  Sealed
                </FieldLabel>
              </Field>
            ) : null}
            {supportsExport ? (
              <Field orientation="horizontal">
                <Checkbox
                  id="new-variable-exported"
                  checked={state.exported}
                  onCheckedChange={(checked) =>
                    dispatch({
                      type: "exportedChanged",
                      checked: Boolean(checked),
                    })
                  }
                />
                <FieldLabel
                  htmlFor="new-variable-exported"
                  className="font-normal"
                >
                  Exported
                </FieldLabel>
              </Field>
            ) : null}
          </FieldGroup>
        ) : null}
        <div className="flex items-center gap-2">
          <Button
            type="button"
            disabled={!state.key.trim() || state.isSubmitting}
            onClick={() => void handleAdd()}
          >
            {state.isSubmitting ? (
              <Spinner data-icon="inline-start" />
            ) : (
              <CheckIcon data-icon="inline-start" />
            )}
            Add
          </Button>
          <Button
            type="button"
            variant="outline"
            disabled={state.isSubmitting}
            onClick={closeForm}
          >
            Cancel
          </Button>
        </div>
      </FieldGroup>

      <ConfirmDialog
        open={state.overwriteCandidate !== null}
        onOpenChange={(open) => {
          if (!open) dispatch({ type: "overwriteCleared" });
        }}
        title="Variable overwrite detected"
        description="This will replace the existing variable’s value."
        actionLabel="Overwrite"
        pendingLabel="Overwriting…"
        variant="destructive"
        onConfirm={handleConfirmOverwrite}
      />
    </>
  );
}
