import { useId, type ReactNode } from "react";
import { ChevronRightIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { Checkbox } from "#/components/ui/checkbox";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "#/components/ui/collapsible";
import { Field, FieldContent, FieldDescription, FieldError, FieldGroup, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { gigabytes, volumeStorage } from "#/modules/config-store/store-volumes";

/** A volume's one Advanced disclosure: whatever most volumes never need, behind a quiet toggle. */
function VolumeAdvanced({ defaultOpen = false, children }: { defaultOpen?: boolean; children: ReactNode }) {
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

/** A Managed volume's limit, with a plain Docker volume behind an explicit Advanced opt-out. */
export function VolumeStorageFields({ managed, sizeGB, onManagedChange, onSizeChange }: {
  managed: boolean;
  sizeGB: string;
  onManagedChange: (managed: boolean) => void;
  onSizeChange: (sizeGB: string) => void;
}) {
  const id = useId();
  const validationError = volumeStorage(managed, sizeGB) ? null
    : "Enter a valid decimal GB limit (at least 0.001, up to 9 decimal places).";
  return (
    <FieldGroup>
      {managed ? <Field data-invalid={validationError ? true : undefined}>
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
      </VolumeAdvanced>
    </FieldGroup>
  );
}
