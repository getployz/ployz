import { useState } from "react";
import { PlusIcon } from "lucide-react";
import { ConfirmableInput } from "#/components/stageable/confirmable-input";
import { Button } from "#/components/ui/button";
import { Field, FieldDescription, FieldLabel } from "#/components/ui/field";

/**
 * An optional command: a button until it has one, which opens an input on click. Confirming saves it (blank unsets
 * it); Escape, or confirming blank, closes back to the button. A staged change shows pink, titled with what's deployed.
 */
export function ServiceCommandField({
  label, description, placeholder, value, baselineValue, isChanged, compact = false, validate, onCommit,
}: {
  label: string;
  description: string;
  placeholder: string;
  value: string | null;
  baselineValue?: string;
  isChanged: boolean;
  /** Collapsed, only a small link-styled button (a command most Services never need). */
  compact?: boolean;
  validate: (raw: string) => string | null;
  onCommit: (value: string | null) => void;
}) {
  // The draft follows `value` until edited; null is the closed button.
  const [draft, setDraft] = useState<{ source: string | null; text: string | null; error: string | null }>({ source: value, text: value, error: null });
  const current = draft.source === value ? draft : { source: value, text: value, error: null };
  const open = current.text !== null;
  const text = current.text ?? "";
  const isDirty = current.text !== null && (value === null || text !== value);

  function confirm() {
    const next = text.trim() || null;
    const error = next === null ? null : validate(next);
    if (error) return setDraft({ ...current, error });
    onCommit(next);
    setDraft({ source: value, text: next, error: null });
  }

  const button = (
    <Button type="button" variant={compact ? "link" : "outline"} size={compact ? "sm" : "default"}
      data-changed={isChanged || undefined} onClick={() => setDraft({ source: value, text: "", error: null })}>
      <PlusIcon data-icon="inline-start" />{label}
    </Button>
  );
  if (!open && compact) return <Field>{button}</Field>;
  return (
    <Field>
      <FieldLabel>{label}</FieldLabel>
      <FieldDescription>{description}</FieldDescription>
      {open ? (
        <ConfirmableInput aria-label={label} aria-invalid={current.error ? true : undefined} error={current.error}
          isChanged={isChanged} isDirty={isDirty} placeholder={placeholder} autoFocus={value === null}
          title={isChanged && baselineValue !== undefined ? `Deployed: ${baselineValue || "none"}` : undefined}
          value={text} onValueChange={(next) => setDraft({ ...current, text: next, error: null })}
          onCancel={() => setDraft({ source: value, text: value, error: null })} onConfirm={confirm} />
      ) : button}
    </Field>
  );
}
