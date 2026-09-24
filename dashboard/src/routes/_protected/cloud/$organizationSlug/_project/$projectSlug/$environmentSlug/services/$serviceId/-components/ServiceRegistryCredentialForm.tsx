import { Schema } from "effect";
import { Button } from "#/components/ui/button";
import { FieldGroup } from "#/components/ui/field";
import {
  appFormOptions,
  showErrorsAfterBlurOrSubmit,
  useAppForm,
  validateOnChangeOrBlur,
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
  validators: [validateOnChangeOrBlur(registryCredentialFormSchema)],
});

type ServiceRegistryCredentialFormProps = {
  usernameLabel: string;
  secretLabel: string;
  description: string;
  initialUsername: string;
  baselineLabel?: string;
  baselineValue?: string;
  isChanged?: boolean;
  onSubmit: (value: { username: string; secret: string }) => void;
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

  const form = useAppForm({
    ...registryCredentialFormOptions,
    defaultValues: {
      username: initialUsername,
      secret: "",
    },
    // Optimistic: saving rolls back and toasts on failure.
    onSubmit: ({ schemaOutputs }) => onSubmit(schemaOutputs[0]),
  });

  function resetForm() {
    form.reset({
      username: initialUsername,
      secret: "",
    });
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
