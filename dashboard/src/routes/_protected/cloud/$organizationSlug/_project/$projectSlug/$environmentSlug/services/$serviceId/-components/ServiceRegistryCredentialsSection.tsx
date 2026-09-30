import { useState } from "react";
import { Button } from "#/components/ui/button";
import {
  Field,
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
import { InfoIcon, KeyRoundIcon, PencilIcon } from "lucide-react";
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

function RegistryCredentialEmptyState({
  description,
  changed,
  actionLabel,
  onAction,
  onRestore,
}: {
  description: string;
  changed: boolean;
  actionLabel: string;
  onAction: () => void;
  onRestore?: () => void;
}) {
  return (
    <Item state="info" data-changed={changed || undefined}>
      <ItemMedia variant="icon">
        <InfoIcon />
      </ItemMedia>
      <ItemContent>
        <ItemDescription>{description}</ItemDescription>
      </ItemContent>
      <ItemActions>
        {onRestore ? (
          <Button type="button" variant="ghost" onClick={onRestore}>
            Restore
          </Button>
        ) : null}
        <Button type="button" variant="outline" onClick={onAction}>
          {actionLabel}
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
          configured ? (
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
            <RegistryCredentialEmptyState
              changed={changed}
              description={`Private image? Add your ${providerLabel} credentials.`}
              actionLabel="Add credentials"
              onAction={() => {
                setMode("create");
              }}
              onRestore={onRestore}
            />
          )
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
