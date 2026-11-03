import { SchemaFieldInput } from "#/components/stageable/schema-field-input";
import { Button } from "#/components/ui/button";
import type { StringSchema } from "#/lib/schema";
import type { Write } from "#/modules/config-store/store-write";

type ServiceRegistryCredentialSingleFieldEditorProps = {
  schema: StringSchema;
  secretLabel: string;
  baselineLabel?: string;
  baselineValue?: string;
  isChanged?: boolean;
  multiline?: boolean;
  rows?: number;
  onCommit: (secret: string) => Write;
  onClose: () => void;
};

export function ServiceRegistryCredentialSingleFieldEditor({
  schema,
  secretLabel,
  baselineLabel,
  baselineValue,
  isChanged = false,
  multiline = false,
  rows,
  onCommit,
  onClose,
}: ServiceRegistryCredentialSingleFieldEditorProps) {
  return (
    <div className="mt-4 flex flex-col gap-2 ph-no-capture">
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
        // Cancel sits beside the field: leaving it for Cancel must not save the token being discarded.
        commit="check"
        onCommit={onCommit}
      />
      <div>
        <Button type="button" variant="ghost" onClick={onClose}>
          Cancel
        </Button>
      </div>
    </div>
  );
}
