import { useState } from "react";
import { Button } from "#/components/ui/button";
import {
  Field,
  FieldContent,
  FieldDescription,
  FieldGroup,
  FieldLabel,
} from "#/components/ui/field";
import {
  Item,
  ItemActions,
  ItemContent,
  ItemDescription,
  ItemMedia,
  ItemTitle,
} from "#/components/ui/item";
import {
  getRegistryHostFromImageReference,
  registryCredentialSecretSchema,
  registryProvider,
} from "#/modules/config-store/registry-credentials";
import { ServiceRegistryCredentialForm } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceRegistryCredentialForm";
import { ServiceRegistryCredentialSingleFieldEditor } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceRegistryCredentialSingleFieldEditor";
import { KeyRoundIcon, PencilIcon } from "lucide-react";
import type { PersistableTransaction } from "#/components/stageable/collection-field-resources";

function RegistryCredentialSummary({
  providerLabel,
  registryHost,
  username,
  title,
  changed,
  onEdit,
  onDelete,
}: {
  providerLabel: string;
  registryHost: string;
  username: string | null;
  title?: string;
  changed: boolean;
  onEdit: () => void;
  onDelete: () => void;
}) {
  return (
    <Item variant="muted" title={title} data-changed={changed || undefined}>
      <ItemMedia variant="icon">
        <KeyRoundIcon />
      </ItemMedia>
      <ItemContent>
        <ItemTitle>Credentials configured</ItemTitle>
        <ItemDescription>
          {providerLabel} · {registryHost}
          {username ? ` · ${username}` : ""}
        </ItemDescription>
      </ItemContent>
      <ItemActions>
        <Button type="button" variant="ghost" size="icon" onClick={onEdit}>
          <PencilIcon />
          <span className="sr-only">Edit registry credentials</span>
        </Button>
        <Button type="button" variant="outline" onClick={onDelete}>
          Disconnect
        </Button>
      </ItemActions>
    </Item>
  );
}

/**
 * A private image's pull credentials: a summary once configured, else a prompt to add them, and the provider's form.
 * Saving is optimistic; the secret is never read back.
 */
export function RegistryCredentialsField({
  label = "Registry Credentials",
  image,
  configured,
  username,
  changed,
  baselineLabel,
  baselineValue,
  onSet,
  onClear,
  onRestore,
}: {
  label?: string;
  image: string;
  configured: boolean;
  /** The saved username, when reads show it. */
  username: string | null;
  changed: boolean;
  baselineLabel?: string;
  baselineValue?: string;
  onSet: (value: { username: string | null; secret: string }) => PersistableTransaction;
  onClear: () => void;
  /** Turns the stored credentials back on, when there are some to restore. */
  onRestore?: () => void;
}) {
  const [mode, setMode] = useState<"create" | "edit" | null>(null);
  const provider = registryProvider(image);
  const providerLabel = provider.label;
  const fixedUsername = provider.fixedUsername;
  const registryHost = getRegistryHostFromImageReference(image);
  const usesSingleFieldUpdater =
    provider.usernameLabel == null || fixedUsername != null;

  // Optimistic: the writer rolls back and toasts if saving fails.
  function handleSubmit(value: { username: string | null; secret: string }) {
    onSet(value);
    setMode(null);
  }

  function handleDelete() {
    onClear();
    setMode(null);
  }

  // Most images are public: none yet is a quiet row, not a prompt.
  if (mode == null && !configured) {
    return (
      <Field orientation="responsive">
        <FieldContent>
          <FieldLabel>{label}</FieldLabel>
          <FieldDescription>For private images.</FieldDescription>
        </FieldContent>
        <div className="flex shrink-0 gap-2">
          {onRestore ? <Button type="button" variant="ghost" onClick={onRestore}>Restore</Button> : null}
          <Button type="button" variant="outline" data-changed={changed || undefined} onClick={() => setMode("create")}>
            Add credentials
          </Button>
        </div>
      </Field>
    );
  }

  return (
    <FieldGroup>
      <Field>
        <div className="flex items-start justify-between gap-3">
          <div className="flex flex-col gap-1">
            <FieldLabel>{label}</FieldLabel>
            <FieldDescription>
              Private Docker Registry credentials used to deploy your Docker
              image.
            </FieldDescription>
          </div>
        </div>

        {mode == null ? (
          <RegistryCredentialSummary
            changed={changed}
            providerLabel={providerLabel}
            registryHost={registryHost}
            username={username}
            title={
              changed && baselineValue != null
                ? `${baselineLabel ?? "Deployed"}: ${baselineValue}`
                : undefined
            }
            onEdit={() => setMode("edit")}
            onDelete={handleDelete}
          />
        ) : (
          <>
            {usesSingleFieldUpdater ? (
              <ServiceRegistryCredentialSingleFieldEditor
                schema={registryCredentialSecretSchema}
                secretLabel={provider.secretLabel}
                description={provider.description}
                baselineLabel={baselineLabel}
                baselineValue={baselineValue}
                isChanged={changed}
                multiline={provider.multiline}
                rows={provider.multiline ? 8 : undefined}
                onCommit={(secret) => {
                  const transaction = onSet({
                    username: fixedUsername,
                    secret: secret.trim(),
                  });
                  setMode(null);
                  return transaction;
                }}
                onClose={() => setMode(null)}
              />
            ) : (
              <ServiceRegistryCredentialForm
                key={`${mode}:${username ?? ""}`}
                usernameLabel={provider.usernameLabel ?? "Username"}
                secretLabel={provider.secretLabel}
                description={provider.description}
                initialUsername={username ?? ""}
                baselineLabel={baselineLabel}
                baselineValue={baselineValue}
                isChanged={changed}
                onSubmit={handleSubmit}
                onClose={() => setMode(null)}
              />
            )}
          </>
        )}
      </Field>
    </FieldGroup>
  );
}
