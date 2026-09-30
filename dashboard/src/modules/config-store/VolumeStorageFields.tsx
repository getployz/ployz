import { useId } from "react";
import { Checkbox } from "#/components/ui/checkbox";
import { Field, FieldContent, FieldDescription, FieldError, FieldGroup, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";

/** The normal limit, with Docker storage behind an explicit Advanced opt-out. */
export function VolumeStorageFields({ managed, sizeGB, onManagedChange, onSizeChange, error }: {
  managed: boolean;
  sizeGB: string;
  onManagedChange: (managed: boolean) => void;
  onSizeChange: (sizeGB: string) => void;
  error?: string | null;
}) {
  const id = useId();
  return (
    <FieldGroup>
      {managed ? <Field data-invalid={error ? true : undefined}>
        <FieldLabel htmlFor={`${id}-limit`}>Storage limit (GB)</FieldLabel>
        <Input id={`${id}-limit`} type="number" min="0.001" step="0.001" max="9007199" required aria-invalid={error ? true : undefined}
          value={sizeGB} onChange={(event) => onSizeChange(event.target.value)} />
        <FieldDescription>The maximum space this volume can use.</FieldDescription>
        {error ? <FieldError>{error}</FieldError> : null}
      </Field> : null}
      <details open={managed ? undefined : true}>
        <summary className="cursor-pointer text-sm">Advanced</summary>
        <Field orientation="horizontal" className="mt-4">
          <Checkbox id={`${id}-managed`} checked={managed} onCheckedChange={onManagedChange} />
          <FieldContent>
            <FieldLabel htmlFor={`${id}-managed`}>Manage storage with Ployz</FieldLabel>
            <FieldDescription>{managed ? "Ployz prepares storage and enforces this volume’s limit."
              : "Uses a Docker volume on the server, without an enforced storage limit."}</FieldDescription>
          </FieldContent>
        </Field>
      </details>
    </FieldGroup>
  );
}
