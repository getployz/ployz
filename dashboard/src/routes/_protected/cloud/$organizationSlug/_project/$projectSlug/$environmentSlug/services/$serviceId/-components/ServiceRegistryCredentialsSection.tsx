import { useState } from "react";
import { Button } from "#/components/ui/button";
import { SettingsRow, SettingsWorkspace } from "#/components/settings/settings-section";
import {
  getRegistryHostFromImageReference,
  registryCredentialSecretSchema,
  registryProvider,
} from "#/modules/config-store/registry-credentials";
import { ServiceRegistryCredentialForm } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceRegistryCredentialForm";
import { ServiceRegistryCredentialSingleFieldEditor } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceRegistryCredentialSingleFieldEditor";
import { PencilIcon } from "lucide-react";
import type { PersistableTransaction } from "#/components/stageable/collection-field-resources";

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
      <SettingsRow label={label} hint="For private images." commit="none" control={
        <>
          {onRestore ? <Button type="button" variant="ghost" onClick={onRestore}>Restore</Button> : null}
          <Button type="button" variant="outline" data-changed={changed || undefined} onClick={() => setMode("create")}>
            Add credentials
          </Button>
        </>
      } />
    );
  }

  if (mode == null) {
    return (
      <SettingsRow label={label} hint={`${providerLabel} · ${registryHost}${username ? ` · ${username}` : ""}`} commit="change" control={
        <>
          <Button type="button" variant="ghost" size="icon" data-changed={changed || undefined}
            title={changed && baselineValue != null ? `${baselineLabel ?? "Deployed"}: ${baselineValue}` : undefined}
            onClick={() => setMode("edit")}>
            <PencilIcon />
            <span className="sr-only">Edit registry credentials</span>
          </Button>
          <Button type="button" variant="outline" onClick={handleDelete}>Disconnect</Button>
        </>
      } />
    );
  }

  return (
    <SettingsWorkspace label={label} hint="Private registry credentials used to pull your image.">
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
    </SettingsWorkspace>
  );
}
