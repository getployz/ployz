import { useState, type ReactNode } from "react";
import { PlusIcon } from "lucide-react";
import { ConfirmableInput } from "#/components/stageable/confirmable-input";
import { Button } from "#/components/ui/button";
import { Field, FieldContent, FieldDescription, FieldLabel } from "#/components/ui/field";

/**
 * An optional command: an input with a placeholder, blank while unset. Confirming saves it (blank unsets it); Escape
 * puts back what's saved. A `compact` one most Services never need stays a small link until opened, and Escape or
 * confirming blank closes it again. A staged change shows pink, titled with what's deployed.
 */
export function ServiceCommandField({
  label, addLabel = label, description, placeholder, value, baselineValue, isChanged, compact = false, note, validate, onCommit,
}: {
  label: string;
  /** The compact link's words; the label by default. */
  addLabel?: string;
  description?: string;
  placeholder: string;
  value: string | null;
  baselineValue?: string;
  isChanged: boolean;
  /** Collapsed, only a small link-styled button (a command most Services never need). */
  compact?: boolean;
  /** Under the hint: what the next Deploy changes here, with Undo. */
  note?: ReactNode;
  validate: (raw: string) => string | null;
  onCommit: (value: string | null) => void;
}) {
  // The draft follows `value` until edited; null is the closed link (compact only).
  const [draft, setDraft] = useState<{ source: string | null; text: string | null; error: string | null }>({ source: value, text: value, error: null });
  const current = draft.source === value ? draft : { source: value, text: value, error: null };
  const open = current.text !== null || !compact;
  const text = current.text ?? "";
  const isDirty = current.text !== null && (value === null || text !== value);

  function confirm() {
    const next = text.trim() || null;
    const error = next === null ? null : validate(next);
    if (error) return setDraft({ ...current, error });
    onCommit(next);
    setDraft({ source: value, text: next, error: null });
  }

  if (!open) {
    return (
      // Left-aligned under the row above: a Field's children are full width, so the link sits in its own box.
      <Field>
        <div>
          <Button type="button" variant="link" size="sm" data-changed={isChanged || undefined}
            onClick={() => setDraft({ source: value, text: "", error: null })}>
            <PlusIcon data-icon="inline-start" />{addLabel}
          </Button>
        </div>
      </Field>
    );
  }
  return (
    <Field orientation="responsive">
      <FieldContent>
        <FieldLabel>{label}</FieldLabel>
        {description ? <FieldDescription>{description}</FieldDescription> : null}
        {note}
      </FieldContent>
      <div className="@md/field-group:shrink-0 @md/field-group:basis-56">
        <ConfirmableInput aria-label={label} aria-invalid={current.error ? true : undefined} error={current.error}
          isChanged={isChanged} isDirty={isDirty} placeholder={placeholder} autoFocus={compact && value === null}
          title={isChanged && baselineValue !== undefined ? `Deployed: ${baselineValue || "none"}` : undefined}
          value={text} onValueChange={(next) => setDraft({ ...current, text: next, error: null })}
          onCancel={() => setDraft({ source: value, text: value, error: null })} onConfirm={confirm} />
      </div>
    </Field>
  );
}
