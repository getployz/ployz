import { Schema } from "effect";
import type { PersistableTransaction } from "#/components/stageable/collection-field-resources";
import { ConfirmableInput } from "#/components/stageable/confirmable-input";
import {
  appFormOptions,
  showErrorsAfterBlurOrSubmit,
  useAppForm,
  validateAfterBlurThenWhileInvalid,
} from "#/form";
import { asRecord, asString } from "#/lib/json";
import {
  strictParseOptions,
  type StringSchema,
} from "#/modules/environment-design/schema";

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

type SchemaFieldInputProps = {
  schema: StringSchema;
  value: string;
  label?: string;
  baselineLabel?: string;
  baselineValue?: string;
  isChanged?: boolean;
  disabled?: boolean;
  placeholder?: string;
  type?: React.ComponentProps<"input">["type"];
  multiline?: boolean;
  rows?: number;
  onCommit: (value: string) => PersistableTransaction;
};

export function SchemaFieldInput({
  schema,
  value,
  label,
  baselineLabel = "Deployed",
  baselineValue,
  isChanged = false,
  disabled,
  placeholder,
  type,
  multiline = false,
  rows,
  onCommit,
}: SchemaFieldInputProps) {
  const formSchema = Schema.toStandardSchemaV1(
    Schema.Struct({ value: schema }),
    { parseOptions: strictParseOptions },
  );
  const formOptions = appFormOptions.strictSchema({
    defaultValues: {
      value,
    },
    errorVisibility: showErrorsAfterBlurOrSubmit,
    validators: [validateAfterBlurThenWhileInvalid(formSchema)],
  });
  const form = useAppForm({
    ...formOptions,
    onSubmit: async ({ schemaOutputs }) => {
      const submittedValue = schemaOutputs[0].value;

      form.reset({
        value: submittedValue,
      });

      try {
        const transaction = onCommit(submittedValue);
        await transaction.isPersisted.promise;
      } catch {
        form.setFieldValue("value", submittedValue);
      }
    },
  });

  return (
    <form.Field name="value">
      {(field) => {
        const error = getFirstFieldError(field.errors);

        return (
          <form.Subscribe selector={(state) => state.isSubmitting}>
            {(isSubmitting) => (
              <ConfirmableInput
                aria-label={label}
                aria-invalid={!!error}
                disabled={disabled}
                error={error}
                isChanged={isChanged}
                isDirty={!field.meta.isDefaultValue}
                isPending={isSubmitting}
                multiline={multiline}
                placeholder={placeholder}
                rows={rows}
                title={
                  isChanged && baselineValue != null
                    ? `${baselineLabel}: ${baselineValue}`
                    : undefined
                }
                type={type}
                value={field.value}
                onValueChange={(nextValue) => field.handleChange(nextValue)}
                onCancel={() => {
                  form.reset({
                    value,
                  });
                }}
                onConfirm={() => {
                  field.handleBlur();
                  void form.handleSubmit().catch(() => undefined);
                }}
              />
            )}
          </form.Subscribe>
        );
      }}
    </form.Field>
  );
}
