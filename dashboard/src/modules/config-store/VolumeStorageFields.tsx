import { useId } from "react";
import { ChevronRightIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { Checkbox } from "#/components/ui/checkbox";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "#/components/ui/collapsible";
import { Field, FieldContent, FieldDescription, FieldError, FieldGroup, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { gigabytes, volumeStorage } from "./store-volumes";

/** A Managed volume's limit, with a plain Docker volume behind an explicit Advanced opt-out. */
export function VolumeStorageFields({ managed, sizeGB, onManagedChange, onSizeChange, error }: {
  managed: boolean;
  sizeGB: string;
  onManagedChange: (managed: boolean) => void;
  onSizeChange: (sizeGB: string) => void;
  error?: string | null;
}) {
  const id = useId();
  const validationError = error ?? (volumeStorage(managed, sizeGB) ? null
    : "Enter a valid decimal GB limit (at least 0.001, up to 9 decimal places).");
  return (
    <FieldGroup>
      {managed ? <Field data-invalid={validationError ? true : undefined}>
        <FieldLabel htmlFor={`${id}-limit`}>Storage limit (GB)</FieldLabel>
        <Input id={`${id}-limit`} type="number" min="0.001" step="any" max={gigabytes(Number.MAX_SAFE_INTEGER)} required aria-invalid={validationError ? true : undefined}
          value={sizeGB} onChange={(event) => onSizeChange(event.target.value)} />
        <FieldDescription>The maximum space this volume can use.</FieldDescription>
        {validationError ? <FieldError>{validationError}</FieldError> : null}
      </Field> : null}
      <Collapsible defaultOpen={!managed} className="flex flex-col gap-4">
        <CollapsibleTrigger render={<Button type="button" variant="ghost" size="sm" className="group self-start" />}>
          Advanced
          <ChevronRightIcon data-icon="inline-end" className="transition-transform group-data-[panel-open]:rotate-90" />
        </CollapsibleTrigger>
        <CollapsibleContent render={<Field orientation="horizontal" />}>
          <Checkbox id={`${id}-docker`} checked={!managed} onCheckedChange={(docker) => onManagedChange(!docker)} />
          <FieldContent>
            <FieldLabel htmlFor={`${id}-docker`}>Use a plain Docker volume (not recommended)</FieldLabel>
            {managed ? null : <FieldDescription>No size limit, and it stays out of backups and Server moves as they arrive.</FieldDescription>}
          </FieldContent>
        </CollapsibleContent>
      </Collapsible>
    </FieldGroup>
  );
}
