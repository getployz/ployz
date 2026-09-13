import { type ReactNode, useState } from "react";
import type { PersistableTransaction } from "#/components/stageable/collection-field-resources";
import { ConfirmableInput } from "#/components/stageable/confirmable-input";

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

  async function confirm() {
    const raw = active.value.trim();
    const error = validate?.(raw) ?? null;
    if (error) {
      setDraft({ ...active, error });
      return;
    }

    setDraft({ ...active, error: null, pending: true });
    try {
      await onCommit(raw).isPersisted.promise;
      setDraft(freshDraft(raw));
    } catch {
      setDraft({ ...active, error: "Could not save", pending: false });
    }
  }

  return (
    <ConfirmableInput
      aria-label={ariaLabel}
      aria-invalid={active.error ? true : undefined}
      inputMode={inputMode}
      type={type}
      min={min}
      max={max}
      step={step}
      suffix={suffix}
      isChanged={isChanged}
      isDirty={isDirty}
      isPending={active.pending}
      error={active.error}
      placeholder={placeholder}
      title={
        isChanged && baselineValue != null
          ? `${baselineLabel}: ${baselineValue}`
          : undefined
      }
      value={active.value}
      onValueChange={(next) =>
        setDraft({ source: value, value: next, error: null, pending: false })
      }
      onCancel={() => setDraft(freshDraft(value))}
      onConfirm={() => {
        void confirm();
      }}
    />
  );
}
