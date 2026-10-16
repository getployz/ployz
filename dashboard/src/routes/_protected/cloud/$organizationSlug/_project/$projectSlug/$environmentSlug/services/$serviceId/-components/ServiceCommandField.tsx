import { useState } from "react";
import { SettingsRow } from "#/components/settings/settings-section";
import type { PersistableTransaction } from "#/components/stageable/collection-field-resources";
import { ConfirmableInput } from "#/components/stageable/confirmable-input";
import { useCommitHold } from "#/components/stageable/use-commit-hold";

/**
 * An optional command: an input with a placeholder, blank while unset. Confirming saves it (blank unsets it); Escape
 * puts back what's saved. A staged change shows pink, titled with what's deployed.
 */
export function ServiceCommandField({ label, description, placeholder, value: stored, baselineValue, isChanged, validate, onCommit }: {
  label: string;
  description?: string;
  placeholder: string;
  value: string | null;
  baselineValue?: string;
  isChanged: boolean;
  validate: (raw: string) => string | null;
  onCommit: (value: string | null) => PersistableTransaction;
}) {
  const [value, hold] = useCommitHold(stored);
  // The draft follows `value` until edited.
  const [draft, setDraft] = useState<{ source: string | null; text: string; error: string | null }>({ source: value, text: value ?? "", error: null });
  const current = draft.source === value ? draft : { source: value, text: value ?? "", error: null };
  const isDirty = current.text !== (value ?? "");

  function confirm() {
    const next = current.text.trim() || null;
    const error = next === null ? null : validate(next);
    if (error) return setDraft({ ...current, error });
    hold(next, onCommit(next));
    setDraft({ source: next, text: next ?? "", error: null });
  }

  return (
    <SettingsRow label={label} hint={description} commit="confirm" control={
      <ConfirmableInput aria-label={label} aria-invalid={current.error ? true : undefined} error={current.error}
        isChanged={isChanged} isDirty={isDirty} placeholder={placeholder}
        title={isChanged && baselineValue !== undefined ? `Deployed: ${baselineValue || "none"}` : undefined}
        value={current.text} onValueChange={(next) => setDraft({ ...current, text: next, error: null })}
        onCancel={() => setDraft({ source: value, text: value ?? "", error: null })} onConfirm={confirm} />
    } />
  );
}
