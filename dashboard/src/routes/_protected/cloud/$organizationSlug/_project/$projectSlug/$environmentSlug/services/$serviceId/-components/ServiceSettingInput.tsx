import { type ReactNode, useState } from "react";
import type { PersistableTransaction } from "#/components/stageable/collection-field-resources";
import { ConfirmableInput } from "#/components/stageable/confirmable-input";
import { SuggestibleConfirmableInput } from "#/components/stageable/suggestible-confirmable-input";

type DraftState = {
  source: string;
  value: string;
  error: string | null;
  pending: boolean;
};

function freshDraft(value: string): DraftState {
  return { source: value, value, error: null, pending: false };
}

/**
 * Inline confirm/cancel text field bound to a single service setting. Commits
 * on confirm via an optimistic `collection.update` transaction and surfaces the
 * per-field "changed vs deployed" affordance. The parent owns value ↔ typed
 * conversion in `validate`/`onCommit`.
 */
export function ServiceSettingInput({
  value,
  isChanged,
  baselineLabel = "Deployed",
  baselineValue,
  placeholder,
  suggestions,
  suggestionsLoading,
  suggestionsMessage,
  suggestionsNotice,
  onFocus,
  inputMode,
  type,
  min,
  max,
  step,
  suffix,
  ariaLabel,
  validate,
  onCommit,
}: {
  value: string;
  isChanged: boolean;
  baselineLabel?: string;
  baselineValue?: string;
  placeholder?: string;
  suggestions?: string[];
  suggestionsLoading?: boolean;
  suggestionsMessage?: string;
  suggestionsNotice?: string;
  onFocus?: () => void;
  inputMode?: "decimal" | "numeric" | "text";
  type?: "number" | "text";
  min?: number;
  max?: number;
  step?: number | "any";
  suffix?: ReactNode;
  ariaLabel: string;
  /** Return an error message to block the commit, or null to allow it. */
  validate?: (raw: string) => string | null;
  onCommit: (raw: string) => PersistableTransaction;
}) {
  const [draft, setDraft] = useState<DraftState>(() => freshDraft(value));
  const active = draft.source === value ? draft : freshDraft(value);
  const isDirty = active.value !== value;

  async function confirm(next = active.value) {
    const raw = next.trim();
    const nextDraft = { ...active, value: raw };
    const error = validate?.(raw) ?? null;
    if (error) {
      setDraft({ ...nextDraft, error });
      return;
    }

    setDraft({ ...nextDraft, error: null, pending: true });
    try {
      await onCommit(raw).isPersisted.promise;
      setDraft(freshDraft(raw));
    } catch {
      setDraft({ ...nextDraft, error: "Could not save", pending: false });
    }
  }

  const inputProps = {
    "aria-label": ariaLabel,
    "aria-invalid": active.error ? true : undefined,
    inputMode,
    type,
    min,
    max,
    step,
    suffix,
    isChanged,
    isDirty,
    isPending: active.pending,
    error: active.error,
    placeholder,
    onFocus,
    title:
      isChanged && baselineValue != null
        ? `${baselineLabel}: ${baselineValue}`
        : undefined,
    value: active.value,
    onValueChange: (next: string) =>
      setDraft({ source: value, value: next, error: null, pending: false }),
    onCancel: () => setDraft(freshDraft(value)),
    onConfirm: () => {
      void confirm();
    },
  };

  if (suggestions) {
    return (
      <SuggestibleConfirmableInput
        {...inputProps}
        suggestions={suggestions}
        suggestionsLoading={suggestionsLoading}
        suggestionsMessage={suggestionsMessage}
        suggestionsNotice={suggestionsNotice}
        onSuggestionSelect={(next) => { void confirm(next); }}
      />
    );
  }

  return <ConfirmableInput {...inputProps} />;
}
