import { Schema } from "effect";
import { Button } from "#/components/ui/button";
import { SettingsFormField } from "#/components/settings/settings-list";
import { FieldError } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import {
  appFormOptions,
  showErrorsAfterBlurOrSubmit,
  useAppForm,
  validateOnChangeOrBlur,
} from "#/form";
import {
  registryCredentialSecretSchema,
  registryCredentialUsernameSchema,
} from "#/modules/config-store/registry-credentials";
import { strictParseOptions } from "#/lib/schema";

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
        <div className="flex flex-col gap-4">
          {(["username", "secret"] as const).map((name) => (
            <form.Field key={name} name={name}>
              {(field) => (
                <SettingsFormField label={name === "username" ? usernameLabel : secretLabel} htmlFor={`registry-${name}`}>
                  <Input
                    id={`registry-${name}`}
                    name={name}
                    type={name === "secret" ? "password" : undefined}
                    placeholder={name === "username" ? usernameLabel : secretLabel}
                    value={field.value}
                    data-changed={isChanged || undefined}
                    title={isChanged && baselineValue != null ? `${baselineLabel}: ${baselineValue}` : undefined}
                    aria-invalid={field.meta.isInvalid || undefined}
                    onBlur={field.handleBlur}
                    onChange={(event) => field.handleChange(event.target.value)}
                  />
                  <FieldError errors={field.errors} />
                </SettingsFormField>
              )}
            </form.Field>
          ))}
          <div className="flex items-center gap-2">
            <form.SubmitButton>Save</form.SubmitButton>
            <Button type="button" variant="outline" onClick={resetForm}>
              Reset
            </Button>
            <Button type="button" variant="ghost" onClick={onClose}>
              Cancel
            </Button>
          </div>
        </div>
      </form.Form>
    </form.AppForm>
  );
}
