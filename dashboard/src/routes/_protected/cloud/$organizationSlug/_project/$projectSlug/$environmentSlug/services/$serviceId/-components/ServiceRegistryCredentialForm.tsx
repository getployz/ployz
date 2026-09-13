import { useState } from "react";
import { Schema } from "effect";
import { Button } from "#/components/ui/button";
import { FieldError, FieldGroup } from "#/components/ui/field";
import {
  appFormOptions,
  showErrorsAfterBlurOrSubmit,
  useAppForm,
  validateAfterBlurThenWhileInvalid,
} from "#/form";
import {
  registryCredentialSecretSchema,
  registryCredentialUsernameSchema,
} from "#/modules/environment-design/services";
import { strictParseOptions } from "#/modules/environment-design/schema";

const registryCredentialFormSchema = Schema.toStandardSchemaV1(
  Schema.Struct({
    username: registryCredentialUsernameSchema,
    secret: registryCredentialSecretSchema,
  }),
  { parseOptions: strictParseOptions },
);

const registryCredentialFormOptions = appFormOptions.strictSchema({
  defaultValues: { username: "", secret: "" },
  errorVisibility: showErrorsAfterBlurOrSubmit,
  validators: [validateAfterBlurThenWhileInvalid(registryCredentialFormSchema)],
});

type ServiceRegistryCredentialFormProps = {
  usernameLabel: string;
  secretLabel: string;
  description: string;
  initialUsername: string;
  baselineLabel?: string;
  baselineValue?: string;
  isChanged?: boolean;
  onSubmit: (value: { username: string; secret: string }) => Promise<void>;
  onClose: () => void;
};

export function ServiceRegistryCredentialForm({
  usernameLabel,
  secretLabel,
  description,
  initialUsername,
  baselineLabel = "Deployed",
  baselineValue,
  isChanged = false,
  onSubmit,
  onClose,
}: ServiceRegistryCredentialFormProps) {
  const [submitError, setSubmitError] = useState<string | null>(null);

  const form = useAppForm({
    ...registryCredentialFormOptions,
    defaultValues: {
      username: initialUsername,
      secret: "",
    },
    listeners: [
      {
        triggers: ["change"],
        run: () => setSubmitError(null),
      },
    ],
    onSubmit: async ({ schemaOutputs }) => {
      setSubmitError(null);

      try {
        await onSubmit(schemaOutputs[0]);
      } catch (error) {
        setSubmitError(
          error instanceof Error
            ? error.message
            : "Could not save registry credentials.",
        );
      }
    },
  });

  function resetForm() {
    form.reset({
      username: initialUsername,
      secret: "",
    });
    setSubmitError(null);
  }

  return (
    <form.AppForm>
      <form.Form className="mt-4">
        <FieldGroup>
          <form.Field name="username">
            {(field) => (
              <field.Text
                label={usernameLabel}
                data-changed={isChanged || undefined}
                placeholder={usernameLabel}
                title={
                  isChanged && baselineValue != null
                    ? `${baselineLabel}: ${baselineValue}`
                    : undefined
                }
              />
            )}
          </form.Field>

          <form.Field name="secret">
            {(field) => (
              <field.Text
                label={secretLabel}
                description={description}
                data-changed={isChanged || undefined}
                placeholder={secretLabel}
                title={
                  isChanged && baselineValue != null
                    ? `${baselineLabel}: ${baselineValue}`
                    : undefined
                }
                type="password"
              />
            )}
          </form.Field>

          {submitError ? <FieldError>{submitError}</FieldError> : null}

          <div className="flex items-center gap-2">
            <form.SubmitButton>Save</form.SubmitButton>
            <Button type="button" variant="outline" onClick={resetForm}>
              Reset
            </Button>
            <Button type="button" variant="ghost" onClick={onClose}>
              Cancel
            </Button>
          </div>
        </FieldGroup>
      </form.Form>
    </form.AppForm>
  );
}
