import { Schema } from "effect";
import { ConfirmableInput } from "#/components/stageable/confirmable-input";
import { Button } from "#/components/ui/button";
import {
  appFormOptions,
  showErrorsAfterBlurOrSubmit,
  useAppForm,
  validateOnChangeOrBlur,
} from "#/form";
import { asRecord, asString } from "#/lib/json";
import { strictParseOptions, type StringSchema } from "#/lib/schema";
import type { Write } from "#/modules/config-store/store-write";

function getFirstFieldError<T>(errors: readonly T[]) {
  for (const error of errors) {
    if (!error) {
      continue;
    }

    const text = asString(error);
    if (text !== null) {
      return text;
    }

    const message = asString(asRecord(error)?.["message"]);
    if (message !== null) {
      return message;
    }
  }

  return null;
}

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
  baselineLabel = "Deployed",
  baselineValue,
  isChanged = false,
  multiline = false,
  rows,
  onCommit,
  onClose,
}: ServiceRegistryCredentialSingleFieldEditorProps) {
  const formSchema = Schema.toStandardSchemaV1(
    Schema.Struct({ value: schema }),
    { parseOptions: strictParseOptions },
  );
  const formOptions = appFormOptions.strictSchema({
    defaultValues: { value: "" },
    errorVisibility: showErrorsAfterBlurOrSubmit,
    validators: [validateOnChangeOrBlur(formSchema)],
  });
  const form = useAppForm({
    ...formOptions,
    onSubmit: ({ schemaOutputs }) => {
      const submittedValue = schemaOutputs[0].value;
      form.reset({ value: submittedValue });
      // Optimistic: a failed save rolls the value back and toasts; show the rolled-back value.
      void onCommit(submittedValue).catch(() => form.reset({ value: "" }));
    },
  });

  return (
    <div className="mt-4 flex flex-col gap-2 ph-no-capture">
      <form.Field name="value">
        {(field) => {
          const error = getFirstFieldError(field.errors);

          return (
            <ConfirmableInput
              aria-label={secretLabel}
              aria-invalid={!!error}
              // Cancel sits beside the field: leaving it for Cancel must not save the token being discarded.
              commit="check"
              error={error}
              isChanged={isChanged}
              isDirty={!field.meta.isDefaultValue}
              multiline={multiline}
              placeholder={secretLabel}
              rows={rows}
              title={
                isChanged && baselineValue != null
                  ? `${baselineLabel}: ${baselineValue}`
                  : undefined
              }
              type={multiline ? undefined : "password"}
              value={field.value}
              onValueChange={(nextValue) => field.handleChange(nextValue)}
              onCancel={() => form.reset({ value: "" })}
              onConfirm={() => {
                field.handleBlur();
                void form.handleSubmit().catch(() => undefined);
              }}
            />
          );
        }}
      </form.Field>
      <div>
        <Button type="button" variant="ghost" onClick={onClose}>
          Cancel
        </Button>
      </div>
    </div>
  );
}
