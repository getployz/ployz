import { useState } from "react";
import { Button } from "#/components/ui/button";
import { SERVICE_DEPLOYMENT_DIFF_PATHS } from "#/modules/services/service-deployment-diff/fields";
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
  getDefaultRegistryCredentialUsername,
  getRegistryHostFromImageReference,
  detectRegistryCredentialProvider,
  getRegistryCredentialProviderHelp,
  getRegistryCredentialProviderLabel,
  registryCredentialSecretSchema,
} from "#/modules/environment-design/services";
import {
  useServiceRegistryCredentialActions,
} from "#/modules/services/services.mutation-actions";
import { ServiceRegistryCredentialForm } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceRegistryCredentialForm";
import type { ServiceDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";
import { ServiceRegistryCredentialSingleFieldEditor } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceRegistryCredentialSingleFieldEditor";
import { InfoIcon, KeyRoundIcon, PencilIcon } from "lucide-react";

function RegistryCredentialSummary({
  providerLabel,
  registryHost,
  username,
  title,
  onEdit,
  onDelete,
}: {
  providerLabel: string;
  registryHost: string;
  username: string | null;
  title?: string;
  onEdit: () => void;
  onDelete: () => void;
}) {
  return (
    <Item variant="muted" title={title}>
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
          Delete
        </Button>
      </ItemActions>
    </Item>
  );
}

function RegistryCredentialEmptyState({
  description,
  actionLabel,
  onAction,
}: {
  description: string;
  actionLabel: string;
  onAction: () => void;
}) {
  return (
    <Item state="info">
      <ItemMedia variant="icon">
        <InfoIcon />
      </ItemMedia>
      <ItemContent>
        <ItemDescription>{description}</ItemDescription>
      </ItemContent>
      <ItemActions>
        <Button type="button" variant="outline" onClick={onAction}>
          {actionLabel}
        </Button>
      </ItemActions>
    </Item>
  );
}

export function ServiceRegistryCredentialsSection({
  state,
}: {
  state: ServiceDrawerState;
}) {
  const { organizationSlug, service, diff } = state;
  const [mode, setMode] = useState<"create" | "edit" | null>(null);

  const source = service.source.type === "image" ? service.source : null;

  const { clearCredentialAction, setCredentialAction } =
    useServiceRegistryCredentialActions({
      organizationSlug,
      environmentId: service.environmentId,
      serviceId: service.id,
    });

  if (!source) {
    return null;
  }

  const credentialsDiff = diff.field(
    SERVICE_DEPLOYMENT_DIFF_PATHS.sourceCredentials,
  );
  const provider = detectRegistryCredentialProvider(source.image);
  const providerLabel = getRegistryCredentialProviderLabel(provider);
  const providerHelp = getRegistryCredentialProviderHelp(provider);
  const fixedUsername = getDefaultRegistryCredentialUsername(provider);
  const registryHost = getRegistryHostFromImageReference(source.image);
  const credentialUsername = service.registryCredentialUsername;
  const hasConfiguredCredential = source.credentials.type === "configured";
  const usesSingleFieldUpdater =
    providerHelp.usernameLabel == null || fixedUsername != null;

  async function handleSubmit(value: {
    username: string | null;
    secret: string;
  }) {
    const transaction = setCredentialAction(value);
    await transaction.isPersisted.promise;
    setMode(null);
  }

  async function handleDelete() {
    const transaction = clearCredentialAction();
    await transaction.isPersisted.promise;
    setMode(null);
  }

  return (
    <FieldGroup>
      <Field>
        <div className="flex items-start justify-between gap-3">
          <div className="flex flex-col gap-1">
            <FieldLabel>Registry Credentials</FieldLabel>
            <FieldDescription>
              Private Docker Registry credentials used to deploy your Docker
              image.
            </FieldDescription>
          </div>
        </div>

        {mode == null ? (
          hasConfiguredCredential ? (
            <RegistryCredentialSummary
              providerLabel={providerLabel}
              registryHost={registryHost}
              username={credentialUsername}
              title={
                credentialsDiff.changed && credentialsDiff.baselineValue != null
                  ? `${credentialsDiff.baselineLabel}: ${credentialsDiff.baselineValue}`
                  : undefined
              }
              onEdit={() => setMode("edit")}
              onDelete={() => {
                void handleDelete();
              }}
            />
          ) : (
            <RegistryCredentialEmptyState
              description={`If you are trying to deploy a private Docker image, please add your ${providerLabel} credentials.`}
              actionLabel="Add credentials"
              onAction={() => {
                setMode("create");
              }}
            />
          )
        ) : (
          <>
            {usesSingleFieldUpdater ? (
              <ServiceRegistryCredentialSingleFieldEditor
                schema={registryCredentialSecretSchema}
                secretLabel={providerHelp.secretLabel}
                description={providerHelp.description}
                baselineLabel={credentialsDiff.baselineLabel}
                baselineValue={credentialsDiff.baselineValue}
                isChanged={credentialsDiff.changed}
                multiline={provider === "gcp-artifact-registry"}
                rows={provider === "gcp-artifact-registry" ? 8 : undefined}
                onCommit={(secret) => {
                  const transaction = setCredentialAction({
                    username: fixedUsername,
                    secret: secret.trim(),
                  });
                  void transaction.isPersisted.promise.then(() => {
                    setMode(null);
                  }, () => undefined);
                  return transaction;
                }}
                onClose={() => setMode(null)}
              />
            ) : (
              <ServiceRegistryCredentialForm
                key={`${mode}:${credentialUsername ?? ""}`}
                usernameLabel={providerHelp.usernameLabel ?? "Username"}
                secretLabel={providerHelp.secretLabel}
                description={providerHelp.description}
                initialUsername={credentialUsername ?? ""}
                baselineLabel={credentialsDiff.baselineLabel}
                baselineValue={credentialsDiff.baselineValue}
                isChanged={credentialsDiff.changed}
                onSubmit={({ username, secret }) =>
                  handleSubmit({
                    username,
                    secret,
                  })
                }
                onClose={() => setMode(null)}
              />
            )}
          </>
        )}
      </Field>
    </FieldGroup>
  );
}
