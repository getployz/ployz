import { useId, type ReactNode } from "react";
import { ChevronRightIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { Checkbox } from "#/components/ui/checkbox";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "#/components/ui/collapsible";
import { Field, FieldContent, FieldDescription, FieldError, FieldGroup, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { gigabytes, volumeStorage } from "./store-volumes";

/** A volume's one Advanced disclosure: whatever most volumes never need, behind a quiet toggle. */
export function VolumeAdvanced({ defaultOpen = false, children }: { defaultOpen?: boolean; children: ReactNode }) {
  return (
    <Collapsible defaultOpen={defaultOpen} className="flex flex-col gap-4">
      <CollapsibleTrigger render={<Button type="button" variant="ghost" size="sm" className="group self-start" />}>
        Advanced
        <ChevronRightIcon data-icon="inline-end" className="transition-transform group-data-[panel-open]:rotate-90" />
      </CollapsibleTrigger>
      <CollapsibleContent className="flex flex-col gap-4">{children}</CollapsibleContent>
    </Collapsible>
  );
}

/**
 * A Managed volume's limit, with a plain Docker volume behind an explicit Advanced opt-out. The create dialog edits a
 * draft; a settings panel passes `limitInput`, its own autosaving input, and gets the limit as a row, and `advanced`,
 * its other rarely-needed settings, which share the one Advanced disclosure.
 */
export function VolumeStorageFields({ managed, sizeGB, onManagedChange, onSizeChange, error, limitInput, advanced }: {
  managed: boolean;
  sizeGB: string;
  onManagedChange: (managed: boolean) => void;
  onSizeChange: (sizeGB: string) => void;
  error?: string | null;
  limitInput?: ReactNode;
  advanced?: ReactNode;
}) {
  const id = useId();
  const validationError = error ?? (volumeStorage(managed, sizeGB) ? null
    : "Enter a valid decimal GB limit (at least 0.001, up to 9 decimal places).");
  return (
    <FieldGroup>
      {managed && limitInput ? (
        <Field orientation="responsive">
          <FieldContent>
            <FieldLabel>Storage limit</FieldLabel>
          </FieldContent>
          <div className="@md/field-group:shrink-0 @md/field-group:basis-56">{limitInput}</div>
        </Field>
      ) : managed ? <Field data-invalid={validationError ? true : undefined}>
        <FieldLabel htmlFor={`${id}-limit`}>Storage limit (GB)</FieldLabel>
        <Input id={`${id}-limit`} type="number" min="0.001" step="any" max={gigabytes(Number.MAX_SAFE_INTEGER)} required aria-invalid={validationError ? true : undefined}
          value={sizeGB} onChange={(event) => onSizeChange(event.target.value)} />
        {validationError ? <FieldError>{validationError}</FieldError> : null}
      </Field> : null}
      <VolumeAdvanced defaultOpen={!managed}>
        <Field orientation="horizontal">
          <Checkbox id={`${id}-docker`} checked={!managed} onCheckedChange={(docker) => onManagedChange(!docker)} />
          <FieldContent>
            <FieldLabel htmlFor={`${id}-docker`}>Use a plain Docker volume (not recommended)</FieldLabel>
            {managed ? null : <FieldDescription>No size limit.</FieldDescription>}
          </FieldContent>
        </Field>
        {advanced}
      </VolumeAdvanced>
    </FieldGroup>
  );
}
