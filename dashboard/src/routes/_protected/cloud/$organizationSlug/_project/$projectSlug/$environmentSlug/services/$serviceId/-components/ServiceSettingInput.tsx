import { type ReactNode, useState } from "react";
import type { PersistableTransaction } from "#/components/stageable/collection-field-resources";
import { ConfirmableInput } from "#/components/stageable/confirmable-input";
import { SuggestibleConfirmableInput } from "#/components/stageable/suggestible-confirmable-input";

type DraftState = {
  source: string;
  value: string;
  error: string | null;
};

function freshDraft(value: string): DraftState {
  return { source: value, value, error: null };
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
  /** The keyboard phones show. The input stays text: a number input hides what was typed from validation. */
  inputMode?: "decimal" | "numeric" | "text";
  suffix?: ReactNode;
  ariaLabel: string;
  /** Return an error message to block the commit, or null to allow it. */
  validate?: (raw: string) => string | null;
  onCommit: (raw: string) => PersistableTransaction;
}) {
  const [draft, setDraft] = useState<DraftState>(() => freshDraft(value));
  // A new value from elsewhere (another tab, the CLI) replaces the field, unless the user is typing in it: then the
  // typing stays and says what it would replace.
  const typing = draft.value !== draft.source;
  const moved = draft.source !== value;
  const active = moved && !typing ? freshDraft(value) : draft;
  const isDirty = active.value !== value;
  const notice = moved && typing ? `Changed elsewhere to ${value || "the default"}: confirm to replace it, or cancel.` : null;

  function confirm(next = active.value) {
    const raw = next.trim();
    const nextDraft = { ...active, value: raw };
    const error = validate?.(raw) ?? null;
    if (error) {
      setDraft({ ...nextDraft, error });
      return;
    }

    // Optimistic: a failed save rolls `value` back and toasts, which resets this draft.
    onCommit(raw);
    setDraft(freshDraft(raw));
  }

  const inputProps = {
    "aria-label": ariaLabel,
    "aria-invalid": active.error ? true : undefined,
    inputMode,
    type: "text",
    suffix,
    isChanged,
    isDirty,
    error: active.error ?? notice,
    placeholder,
    onFocus,
    title:
      isChanged && baselineValue != null
        ? `${baselineLabel}: ${baselineValue}`
        : undefined,
    value: active.value,
    onValueChange: (next: string) =>
      setDraft({ source: value, value: next, error: null }),
    onCancel: () => setDraft(freshDraft(value)),
    onConfirm: () => confirm(),
  };

  if (suggestions) {
    return (
      <SuggestibleConfirmableInput
        {...inputProps}
        suggestions={suggestions}
        suggestionsLoading={suggestionsLoading}
        suggestionsMessage={suggestionsMessage}
        suggestionsNotice={suggestionsNotice}
        onSuggestionSelect={(next) => confirm(next)}
      />
    );
  }

  return <ConfirmableInput {...inputProps} />;
}
