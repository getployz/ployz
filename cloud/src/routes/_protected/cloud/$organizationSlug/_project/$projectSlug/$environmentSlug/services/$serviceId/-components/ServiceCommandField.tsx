import { useState } from "react";
import type { PersistableTransaction } from "#/components/stageable/collection-field-resources";
import { ConfirmableInput } from "#/components/stageable/confirmable-input";
import { Button } from "#/components/ui/button";
import {
  Field,
  FieldDescription,
  FieldLabel,
} from "#/components/ui/field";
import { PlusIcon } from "lucide-react";
import { Result, Schema } from "effect";
import { serviceCommandSchema } from "#/modules/environment-design/services";
import { strictParseOptions } from "#/modules/environment-design/schema";

type CommandFieldDraftState = {
  sourceValue: string | null;
  draft: string | null;
  error: string | null;
  isPending: boolean;
};

function createCommandFieldDraftState(
  sourceValue: string | null,
): CommandFieldDraftState {
  return {
    sourceValue,
    draft: sourceValue,
    error: null,
    isPending: false,
  };
}

export function ServiceCommandField({
  label,
  description,
  addLabel,
  placeholder,
  value,
  baselineLabel = "Deployed",
  baselineValue,
  isChanged,
  addButtonVariant = "outline",
  addButtonSize = "default",
  collapsedAppearance = "full",
  onCommit,
}: {
  label: string;
  description: string;
  addLabel: string;
  placeholder: string;
  value: string | null;
  baselineLabel?: string;
  baselineValue?: string;
  isChanged: boolean;
  addButtonVariant?: "outline" | "link";
  addButtonSize?: "default" | "sm";
  collapsedAppearance?: "full" | "compact";
  onCommit: (value: string | null) => PersistableTransaction;
}) {
  const [draftState, setDraftState] = useState<CommandFieldDraftState>(() =>
    createCommandFieldDraftState(value),
  );
  const activeDraftState =
    draftState.sourceValue === value
      ? draftState
      : createCommandFieldDraftState(value);
  const { draft, error, isPending } = activeDraftState;
  const updateDraftState = (
    updater: (state: CommandFieldDraftState) => CommandFieldDraftState,
  ) => {
    setDraftState((state) =>
      updater(
        state.sourceValue === value
          ? state
          : createCommandFieldDraftState(value),
      ),
    );
  };

  const isOpen = draft != null;
  const draftValue = draft ?? "";
  const isDirty = draft != null && (value == null ? true : draftValue !== value);

  async function handleConfirm() {
    if (draft == null) {
      return;
    }

    const trimmedValue = draft.trim();
    const nextValue = trimmedValue.length === 0 ? null : trimmedValue;

    if (nextValue != null) {
      const result = Schema.decodeUnknownResult(serviceCommandSchema)(
        nextValue,
        strictParseOptions,
      );

      if (Result.isFailure(result)) {
        updateDraftState((state) => ({
          ...state,
          error:
            result.failure instanceof Error
              ? result.failure.message
              : "Invalid value",
        }));
        return;
      }
    }

    updateDraftState((state) => ({
      ...state,
      error: null,
      isPending: true,
    }));

    try {
      const transaction = onCommit(nextValue);
      await transaction.isPersisted.promise;
      updateDraftState((state) => ({
        ...state,
        draft: nextValue,
        error: null,
        isPending: false,
      }));
    } catch {
      updateDraftState((state) => ({
        ...state,
        error: "Could not save command",
        isPending: false,
      }));
    }
  }

  function handleCancel() {
    setDraftState(createCommandFieldDraftState(value));
  }

  if (!isOpen && collapsedAppearance === "compact") {
    return (
      <Field>
        <Button
          type="button"
          variant={addButtonVariant}
          size={addButtonSize}
          onClick={() => {
            setDraftState({
              sourceValue: value,
              draft: "",
              error: null,
              isPending: false,
            });
          }}
        >
          <PlusIcon data-icon="inline-start" />
          {addLabel}
        </Button>
      </Field>
    );
  }

  return (
    <Field>
      <FieldLabel>{label}</FieldLabel>
      <FieldDescription>{description}</FieldDescription>
      {isOpen ? (
        <ConfirmableInput
          aria-label={label}
          aria-invalid={error ? true : undefined}
          disabled={isPending}
          error={error}
          isChanged={isChanged}
          isDirty={isDirty}
          isPending={isPending}
          placeholder={placeholder}
          title={
            isChanged && baselineValue != null
              ? `${baselineLabel}: ${baselineValue}`
              : undefined
          }
          value={draftValue}
          onValueChange={(nextValue) => {
            updateDraftState((state) => ({
              ...state,
              draft: nextValue,
              error: null,
            }));
          }}
          onCancel={handleCancel}
          onConfirm={() => {
            void handleConfirm();
          }}
        />
      ) : (
        <Button
          type="button"
          variant={addButtonVariant}
          size={addButtonSize}
          onClick={() => {
            setDraftState({
              sourceValue: value,
              draft: "",
              error: null,
              isPending: false,
            });
          }}
        >
          <PlusIcon data-icon="inline-start" />
          {addLabel}
        </Button>
      )}
    </Field>
  );
}
