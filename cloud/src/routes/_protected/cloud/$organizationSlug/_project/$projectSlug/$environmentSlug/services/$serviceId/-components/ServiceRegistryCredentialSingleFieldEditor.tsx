import { SchemaFieldInput } from "#/components/stageable/schema-field-input";
import { Button } from "#/components/ui/button";
import { FieldDescription } from "#/components/ui/field";
import type { PersistableTransaction } from "#/components/stageable/collection-field-resources";
import type { StringSchema } from "#/modules/environment-design/schema";

type ServiceRegistryCredentialSingleFieldEditorProps = {
  schema: StringSchema;
  secretLabel: string;
  description: string;
  baselineLabel?: string;
  baselineValue?: string;
  isChanged?: boolean;
  multiline?: boolean;
  rows?: number;
  onCommit: (secret: string) => PersistableTransaction;
  onClose: () => void;
};

export function ServiceRegistryCredentialSingleFieldEditor({
  schema,
  secretLabel,
  description,
  baselineLabel,
  baselineValue,
  isChanged = false,
  multiline = false,
  rows,
  onCommit,
  onClose,
}: ServiceRegistryCredentialSingleFieldEditorProps) {
  return (
    <div className="mt-4 flex flex-col gap-2">
      <SchemaFieldInput
        schema={schema}
        value=""
        label={secretLabel}
        baselineLabel={baselineLabel}
        baselineValue={baselineValue}
        isChanged={isChanged}
        multiline={multiline}
        placeholder={secretLabel}
        rows={rows}
        type={multiline ? undefined : "password"}
        onCommit={onCommit}
      />
      <FieldDescription>{description}</FieldDescription>
      <div>
        <Button type="button" variant="ghost" onClick={onClose}>
          Cancel
        </Button>
      </div>
    </div>
  );
}
