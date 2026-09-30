import { useReducer } from "react";
import { CheckIcon } from "lucide-react";
import { toast } from "sonner";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { Button } from "#/components/ui/button";
import { Checkbox } from "#/components/ui/checkbox";
import { Field, FieldError, FieldGroup, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { VariableValueInput } from "#/components/variables/VariableValueInput";
import type { ReferenceTarget } from "#/modules/variables/variable-autocomplete";
import { getSealedVariableCollisionMessage } from "#/modules/variables/variable-raw-editor";
import { parseDisplayToParts } from "#/modules/variables/variable-template";
import type { VariableRecord } from "#/modules/variables/variables";

/** A variable's name, as the Store takes it (the settings catalog's `env` keys). */
const VARIABLE_KEY = /^[A-Z_][A-Z0-9_]{0,127}$/u;
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
  /** The key names an existing variable: confirm before replacing it. */
  confirmingOverwrite: boolean;
};

type VariableAddFormAction =
  | { type: "keyChanged"; value: string }
  | { type: "valueChanged"; value: string }
  | { type: "sealedChanged"; checked: boolean }
  | { type: "exportedChanged"; checked: boolean }
  | { type: "overwriteRequested" }
  | { type: "overwriteCleared" }
  | { type: "reset"; defaults: VariableAddFormDefaults };

function createVariableAddFormState({
  allowSealOnCreate,
  defaultExported,
}: VariableAddFormDefaults, draft?: VariableAddInput): VariableAddFormState {
  return {
    key: draft?.key ?? "",
    value: draft?.value ?? "",
    sealed: draft?.sealed ?? allowSealOnCreate,
    exported: draft?.exported ?? defaultExported,
    confirmingOverwrite: false,
  };
}

/** Why `value` can't be saved: a `${{ service.KEY }}` names no Service of the Environment (`serviceNames`). */
function referenceError(value: string, serviceNames: readonly string[] | undefined) {
  if (serviceNames === undefined) return null;
  const [unknown] = parseDisplayToParts(value, (slug) => serviceNames.includes(slug) ? { lineageId: slug, scope: "service" } : null).unresolved;
  return unknown === undefined ? null : `There's no service named ${unknown} to reference in this environment.`;
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
    case "overwriteRequested":
      return { ...state, confirmingOverwrite: true };
    case "overwriteCleared":
      return { ...state, confirmingOverwrite: false };
    case "reset":
      return createVariableAddFormState(action.defaults);
  }
}

export function VariableAddForm({
  variables,
  onCreateVariable,
  onCancel,
  allowSealOnCreate,
  defaultExported,
  supportsExport,
  valueTargets,
  serviceNames,
  initial,
}: {
  variables: VariableRecord[];
  /** Saves in the background; a refusal reopens the form with `initial`. */
  onCreateVariable: (input: VariableAddInput) => void;
  onCancel: () => void;
  /** What was typed before a refusal. */
  initial?: VariableAddInput;
  allowSealOnCreate: boolean;
  defaultExported: boolean;
  supportsExport: boolean;
  valueTargets?: ReferenceTarget[];
  /** Every Service of the Environment, which a value's `${{ service.KEY }}` may name; none: unchecked. */
  serviceNames?: readonly string[];
}) {
  const defaults = { allowSealOnCreate, defaultExported };
  const [state, dispatch] = useReducer(
    variableAddFormReducer,
    defaults,
    (initialDefaults) => createVariableAddFormState(initialDefaults, initial),
  );

  function closeForm() {
    dispatch({ type: "reset", defaults });
    onCancel();
  }

  const typedKey = state.key.trim().toUpperCase();
  const keyError = typedKey && !VARIABLE_KEY.test(typedKey)
    ? "Use letters, digits and underscores, not starting with a digit (at most 128)." : null;
  const valueError = state.sealed
    ? state.value.includes("${{") ? "A sealed value is stored as-is. Untick Sealed to use a reference." : null
    : referenceError(state.value, serviceNames);

  function handleAdd() {
    const key = typedKey;
    // Invalid input stays in the form, as typed, with the reason under it.
    if (!key || keyError || valueError) return;

    const existing = variables.find((variable) => variable.key === key);
    if (existing) {
      if (existing.value.type === "sealed") {
        toast.error(getSealedVariableCollisionMessage(existing.key));
        return;
      }
      dispatch({ type: "overwriteRequested" });
      return;
    }

    save();
  }

  /** Writes the variable as the form has it, sealed or not; an overwrite replaces the existing one. */
  function save() {
    // Optimistic: the writer rolls back and toasts if saving fails.
    onCreateVariable({
      key: typedKey,
      value: state.value,
      sealed: state.sealed,
      exported: state.exported,
    });
    closeForm();
  }

  return (
    <>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          handleAdd();
        }}
      >
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
            className="font-mono text-xs uppercase"
            aria-invalid={keyError ? true : undefined}
          />
          {keyError ? <FieldError>{keyError}</FieldError> : null}
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
          {valueError ? <FieldError>{valueError}</FieldError> : null}
        </Field>
        {allowSealOnCreate || supportsExport ? (
          <FieldGroup>
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
                <FieldLabel htmlFor="new-variable-sealed">
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
                >
                  Exported
                </FieldLabel>
              </Field>
            ) : null}
          </FieldGroup>
        ) : null}
        <div className="flex items-center gap-2">
          <Button
            type="submit"
            disabled={!typedKey || keyError !== null || valueError !== null}
          >
            <CheckIcon data-icon="inline-start" />
            Add
          </Button>
          <Button
            type="button"
            variant="outline"
            onClick={closeForm}
          >
            Cancel
          </Button>
        </div>
      </FieldGroup>
      </form>

      <ConfirmDialog
        open={state.confirmingOverwrite}
        onOpenChange={(open) => {
          if (!open) dispatch({ type: "overwriteCleared" });
        }}
        title="Variable overwrite detected"
        description="This will replace the existing variable’s value."
        actionLabel="Overwrite"
        pendingLabel="Overwriting…"
        variant="destructive"
        onConfirm={save}
      />
    </>
  );
}
