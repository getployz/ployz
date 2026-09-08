import { useState, type ReactNode } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import {
  CircleAlertIcon,
  CopyIcon,
  GlobeIcon,
  PencilIcon,
  PlusIcon,
  Trash2Icon,
  ZapIcon,
} from "lucide-react";
import { toast } from "sonner";
import { Result, Schema, SchemaGetter } from "effect";
import type { ServiceManagedHostname, ServiceRoute } from "#/modules/environment-design/tables";
import {
  Alert,
  AlertAction,
  AlertDescription,
  AlertTitle,
} from "#/components/ui/alert";
import { Button } from "#/components/ui/button";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import {
  Field,
  FieldDescription,
  FieldGroup,
  FieldLabel,
} from "#/components/ui/field";
import { Spinner } from "#/components/ui/spinner";
import {
  appFormOptions,
  showErrorsAfterBlurOrSubmit,
  useAppForm,
  validateAfterBlurThenWhileInvalid,
} from "#/form";
import { cn } from "#/lib/utils";
import {
  billingKeys,
  billingStateQueryOptions,
} from "#/modules/billing/billing.queries";
import { SERVICE_DEPLOYMENT_DIFF_PATHS } from "#/modules/services/service-deployment-diff/fields";
import {
  serviceManagedHostnameSchema,
  serviceManagedHostnamePrefixSchema,
  servicePrivateDnsSchema,
} from "#/modules/environment-design/services";
import { strictParseOptions } from "#/modules/environment-design/schema";
import { useRuntimeStatus } from "#/providers/runtime-provider";
import { customDomainCapabilityPresentation } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceNetworkingSection.presentation";
import { ServiceSettingInput } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceSettingInput";
import type { ServiceDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";
import { CustomDomainDialog } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/CustomDomainDialog";

function copyToClipboard(value: string) {
  void navigator.clipboard.writeText(value);
  toast.info("Copied to clipboard");
}

/** Derive a valid managed-domain prefix from the service's private DNS name. */
function defaultPrefix(privateDns: string) {
  const label = privateDns
    .toLowerCase()
    .replace(/[^a-z0-9-]/g, "-")
    .replace(/^-+|-+$/g, "");
  return label.length > 0 ? label : "app";
}

export function ServiceNetworkingSection({
  state,
}: {
  state: ServiceDrawerState;
}) {
  const { service, collection, diff, managedPrefixesInUse, defaultTargetPort } =
    state;
  const routesDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.routes);
  const managedDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.managedHostname);
  const privateDnsDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.privateDns);
  const routes = service.routes;
  const managed = service.managedHostname;
  const queryClient = useQueryClient();
  const billingQuery = useQuery(
    billingStateQueryOptions(state.organizationSlug),
  );
  const customDomainCapability = customDomainCapabilityPresentation(
    billingQuery.isPending
      ? { status: "loading" }
      : billingQuery.error !== null || billingQuery.data === undefined
        ? { status: "unavailable" }
        : {
            status: "ready",
            billingMode: billingQuery.data.billingMode,
            currentPlan: billingQuery.data.currentPlan,
            hasActivePaidSubscription:
              billingQuery.data.hasActivePaidSubscription,
          },
  );
  const canChangeCustomDomains = customDomainCapability.canAddOrReplace;

  const runtimeStatus = useRuntimeStatus();
  const hostedDnsHostname = runtimeStatus.hostedDnsHostname;
  const hostedDnsHostnameIsCurrent = runtimeStatus.lensStatus === "observed";
  const canUseManaged =
    hostedDnsHostnameIsCurrent && hostedDnsHostname !== null;
  const managedHostname =
    managed != null && hostedDnsHostname != null
      ? `${managed.prefix}.${hostedDnsHostname}`
      : null;
  const certificateEvidence = (hostname: string): DomainCertificateEvidence => {
    const certificate = runtimeStatus.certificates.find(
      (candidate) => candidate.hostname === hostname,
    );
    const incomplete = runtimeStatus.incompleteIds.certificates.includes(hostname);
    if (!certificate && !incomplete) return null;
    return {
      status: certificate?.status ?? null,
      lastObserved: !hostedDnsHostnameIsCurrent,
      incomplete,
    };
  };

  const [editing, setEditing] = useState<
    | { kind: "managed" }
    | { kind: "route"; index: number }
    | { kind: "add" }
    | null
  >(null);

  async function commitRoutes(next: ServiceRoute[]): Promise<void> {
    await collection.update(service.id, (draft) => {
      draft.routes = next;
    }).isPersisted.promise;
  }

  function refreshBillingState() {
    return queryClient.invalidateQueries({
      queryKey: billingKeys.state(state.organizationSlug),
      refetchType: "active",
    });
  }

  function setManaged(next: ServiceManagedHostname | null) {
    void collection.update(service.id, (draft) => {
      draft.managedHostname = next;
    }).isPersisted.promise;
  }

  const hasAnyDomain = managed != null || routes.length > 0;
  const routeBeingEdited =
    editing?.kind === "route" ? routes[editing.index] : undefined;

  return (
    <FieldGroup>
      <Field
        data-changed={managedDiff.changed || routesDiff.changed || undefined}
      >
        <FieldLabel>Public Networking</FieldLabel>
        <FieldDescription>
          Access this service publicly over HTTP through the domains below.
        </FieldDescription>

        <div className="flex flex-col gap-2">
          {!hasAnyDomain ? (
            <Empty className="py-6">
              <EmptyDescription>No public domains yet.</EmptyDescription>
            </Empty>
          ) : null}

          {managed != null ? (
            <ManagedDomainRow
              managed={managed}
              hostedDnsHostname={hostedDnsHostname}
              hostedDnsHostnameIsCurrent={hostedDnsHostnameIsCurrent}
              certificateEvidence={
                managedHostname ? certificateEvidence(managedHostname) : null
              }
              takenPrefixes={managedPrefixesInUse}
              defaultTargetPort={defaultTargetPort}
              changed={managedDiff.changed}
              editing={editing?.kind === "managed"}
              onEdit={() => setEditing({ kind: "managed" })}
              onCancel={() => setEditing(null)}
              onDelete={() => {
                setManaged(null);
                setEditing(null);
              }}
              onSubmit={(next) => {
                setManaged(next);
                setEditing(null);
              }}
            />
          ) : null}

          {routes.map((route, index) => (
            <CustomDomainRow
              key={`${route.hostname}:${route.targetPort}`}
              route={route}
              certificateEvidence={certificateEvidence(route.hostname)}
              changed={routesDiff.changed}
              canEdit={canChangeCustomDomains}
              onEdit={() => setEditing({ kind: "route", index })}
              onDelete={() => {
                commitRoutes(routes.filter((_, i) => i !== index));
                setEditing(null);
              }}
            />
          ))}
        </div>

        {customDomainCapability.status === "blocked" ? (
          <Alert>
            {billingQuery.isPending ? <Spinner /> : <CircleAlertIcon />}
            <AlertTitle>Custom domain access</AlertTitle>
            <AlertDescription>
              {customDomainCapability.message}
            </AlertDescription>
            {customDomainCapability.showUpgrade ? (
              <AlertAction>
                <Button
                  nativeButton={false}
                  render={
                    <Link
                      to="/cloud/$organizationSlug/~/billing"
                      params={{ organizationSlug: state.organizationSlug }}
                    />
                  }
                  size="sm"
                >
                  Upgrade to Solo
                </Button>
              </AlertAction>
            ) : null}
          </Alert>
        ) : null}

        <div className="flex flex-wrap gap-2">
          {canUseManaged && managed == null ? (
            <Button
              type="button"
              variant="outline"
              onClick={() =>
                setManaged({
                  prefix: defaultPrefix(service.privateDns),
                  targetPort: defaultTargetPort,
                })
              }
            >
              <ZapIcon data-icon="inline-start" />
              Generate Domain
            </Button>
          ) : null}
          <Button
            type="button"
            variant="outline"
            disabled={!canChangeCustomDomains}
            onClick={() => setEditing({ kind: "add" })}
          >
            <PlusIcon data-icon="inline-start" />
            Custom Domain
          </Button>
        </div>

        {editing?.kind === "add" ? (
          <CustomDomainDialog
            key="add-custom-domain"
            defaultTargetPort={defaultTargetPort}
            capabilityAction={
              <Button
                nativeButton={false}
                render={
                  <Link
                    to="/cloud/$organizationSlug/~/billing"
                    params={{ organizationSlug: state.organizationSlug }}
                  />
                }
                size="sm"
              >
                Review billing
              </Button>
            }
            onCapabilityRejected={refreshBillingState}
            onClose={() => setEditing(null)}
            onSubmit={(next) => commitRoutes([...routes, next])}
          />
        ) : null}
        {editing?.kind === "route" && routeBeingEdited !== undefined ? (
          <CustomDomainDialog
            key={`edit-custom-domain-${editing.index}`}
            route={routeBeingEdited}
            defaultTargetPort={defaultTargetPort}
            capabilityAction={
              <Button
                nativeButton={false}
                render={
                  <Link
                    to="/cloud/$organizationSlug/~/billing"
                    params={{ organizationSlug: state.organizationSlug }}
                  />
                }
                size="sm"
              >
                Review billing
              </Button>
            }
            onCapabilityRejected={refreshBillingState}
            onClose={() => setEditing(null)}
            onSubmit={(next) =>
              commitRoutes(
                routes.map((currentRoute, index) =>
                  index === editing.index ? next : currentRoute,
                ),
              )
            }
          />
        ) : null}
      </Field>

      <Field>
        <FieldLabel>Private DNS name</FieldLabel>
        <FieldDescription>
          Used as the runtime service ID and internal DNS name.
        </FieldDescription>
        <ServiceSettingInput
          ariaLabel="Private DNS name"
          placeholder="kin-server"
          value={service.privateDns}
          isChanged={privateDnsDiff.changed}
          baselineLabel={privateDnsDiff.baselineLabel}
          baselineValue={privateDnsDiff.baselineValue}
          validate={(raw) => {
            const parsed = Schema.decodeUnknownResult(servicePrivateDnsSchema)(
              raw,
              strictParseOptions,
            );
            return Result.isFailure(parsed)
              ? parsed.failure instanceof Error
                ? parsed.failure.message
                : "Invalid value"
              : null;
          }}
          onCommit={(raw) =>
            collection.update(service.id, (draft) => {
              draft.privateDns = raw;
            })
          }
        />
      </Field>
    </FieldGroup>
  );
}

function DomainRowShell({
  icon,
  children,
  changed,
  actions,
}: {
  icon: ReactNode;
  children: ReactNode;
  changed?: boolean;
  actions: ReactNode;
}) {
  return (
    <div
      className={cn(
        "flex items-center gap-3 rounded-lg border bg-card p-3",
        changed && "border-changed-border bg-changed-soft",
      )}
    >
      <span className="text-muted-foreground">{icon}</span>
      <div className="min-w-0 flex-1">{children}</div>
      <div className="flex items-center gap-1">{actions}</div>
    </div>
  );
}

type DomainCertificateEvidence =
  | {
      status: string | null;
      lastObserved: boolean;
      incomplete: boolean;
    }
  | null;

function ManagedDomainRow({
  managed,
  hostedDnsHostname,
  hostedDnsHostnameIsCurrent,
  certificateEvidence,
  takenPrefixes,
  defaultTargetPort,
  changed,
  editing,
  onEdit,
  onCancel,
  onDelete,
  onSubmit,
}: {
  managed: ServiceManagedHostname;
  hostedDnsHostname: string | null;
  hostedDnsHostnameIsCurrent: boolean;
  certificateEvidence: DomainCertificateEvidence;
  takenPrefixes: string[];
  defaultTargetPort: number;
  changed: boolean;
  editing: boolean;
  onEdit: () => void;
  onCancel: () => void;
  onDelete: () => void;
  onSubmit: (next: ServiceManagedHostname) => void;
}) {
  const hostname = hostedDnsHostname
    ? `${managed.prefix}.${hostedDnsHostname}`
    : `${managed.prefix}.…`;
  const port = managed.targetPort ?? defaultTargetPort;

  if (editing) {
    return (
      <ManagedDomainForm
        managed={managed}
        hostedDnsHostname={hostedDnsHostname}
        takenPrefixes={takenPrefixes}
        defaultTargetPort={defaultTargetPort}
        onCancel={onCancel}
        onSubmit={onSubmit}
      />
    );
  }

  return (
    <div className="flex flex-col gap-1">
      <DomainRowShell
        changed={changed}
        icon={<GlobeIcon />}
        actions={
          <>
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label="Copy domain"
              onClick={() => copyToClipboard(hostname)}
            >
              <CopyIcon />
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label="Edit managed domain"
              onClick={onEdit}
            >
              <PencilIcon />
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label="Remove managed domain"
              onClick={onDelete}
            >
              <Trash2Icon />
            </Button>
          </>
        }
      >
        <div className="truncate font-mono text-sm">{hostname}</div>
        <div className="text-muted-foreground text-sm">→ Port {port}</div>
      </DomainRowShell>
      <FieldDescription>
        {hostedDnsHostname
          ? `${hostedDnsHostnameIsCurrent ? "Hosted DNS hostname observed" : "Hosted DNS hostname last observed"}: ${hostedDnsHostname}.`
          : "Hosted DNS hostname has not been observed."}
      </FieldDescription>
      <CertificateEvidence evidence={certificateEvidence} />
    </div>
  );
}

function CertificateEvidence({
  evidence,
}: {
  evidence: DomainCertificateEvidence;
}) {
  if (!evidence) return null;

  return (
    <FieldDescription>
      {evidence.status
        ? `${evidence.lastObserved ? "Last observed" : "Observed"} certificate status: ${evidence.status.replaceAll("_", " ")}.`
        : "Certificate status was not observed."}
      {evidence.incomplete
        ? " This observation also lists this certificate as incomplete."
        : null}
    </FieldDescription>
  );
}

const portStringSchema = Schema.String.check(
  Schema.makeFilter<string>((value) => {
    const trimmed = value.trim();
    if (trimmed === "") return undefined;
    const port = Number(trimmed);
    return Number.isInteger(port) && port >= 1 && port <= 65535
      ? undefined
      : "Enter a port between 1 and 65535, or leave blank.";
  }),
);

function ManagedDomainForm({
  managed,
  hostedDnsHostname,
  takenPrefixes,
  defaultTargetPort,
  onCancel,
  onSubmit,
}: {
  managed: ServiceManagedHostname;
  hostedDnsHostname: string | null;
  takenPrefixes: string[];
  defaultTargetPort: number;
  onCancel: () => void;
  onSubmit: (next: ServiceManagedHostname) => void;
}) {
  const taken = new Set(takenPrefixes);
  const schema = Schema.toStandardSchemaV1(
    Schema.Struct({
      prefix: serviceManagedHostnamePrefixSchema.check(
        Schema.makeFilter<string>((value) =>
          taken.has(value) ? "This subdomain is already in use." : undefined,
        ),
      ),
      port: portStringSchema,
    })
      .pipe(
        Schema.decodeTo(serviceManagedHostnameSchema, {
          decode: SchemaGetter.transform(({ prefix, port }) => ({
            prefix,
            targetPort: port.trim() === "" ? null : Number(port),
          })),
          encode: SchemaGetter.transform(({ prefix, targetPort }) => ({
            prefix,
            port: targetPort === null ? "" : String(targetPort),
          })),
        }),
    ),
    { parseOptions: strictParseOptions },
  );

  const formOptions = appFormOptions.strictSchema({
    defaultValues: {
      prefix: managed.prefix,
      port: String(managed.targetPort ?? defaultTargetPort),
    },
    errorVisibility: showErrorsAfterBlurOrSubmit,
    validators: [validateAfterBlurThenWhileInvalid(schema)],
  });
  const form = useAppForm({
    ...formOptions,
    onSubmit: ({ schemaOutputs }) => {
      onSubmit(schemaOutputs[0]);
    },
  });

  return (
    <form.AppForm>
      <form.Form className="flex flex-col gap-2 rounded-lg border bg-card p-3">
        <form.Field name="prefix">
          {(field) => (
            <field.Text
              label="Subdomain"
              className="font-mono"
              description={`.${hostedDnsHostname ?? "…"}`}
            />
          )}
        </form.Field>
        <form.Field name="port">
          {(field) => (
            <field.Text
              label="Target port"
              inputMode="numeric"
              placeholder={String(defaultTargetPort)}
              description="The port your app listens on."
            />
          )}
        </form.Field>
        <div className="flex justify-end gap-2">
          <Button type="button" variant="outline" size="sm" onClick={onCancel}>
            Cancel
          </Button>
          <form.SubmitButton size="sm">Update</form.SubmitButton>
        </div>
      </form.Form>
    </form.AppForm>
  );
}

function CustomDomainRow({
  route,
  certificateEvidence,
  changed,
  canEdit,
  onEdit,
  onDelete,
}: {
  route: ServiceRoute;
  certificateEvidence: DomainCertificateEvidence;
  changed: boolean;
  canEdit: boolean;
  onEdit: () => void;
  onDelete: () => void;
}) {
  return (
    <div className="flex flex-col gap-1">
      <DomainRowShell
        changed={changed}
        icon={<GlobeIcon />}
        actions={
          <>
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label={`Copy ${route.hostname}`}
              onClick={() => copyToClipboard(route.hostname)}
            >
              <CopyIcon />
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label={`Edit ${route.hostname}`}
              disabled={!canEdit}
              onClick={onEdit}
            >
              <PencilIcon />
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label={`Remove ${route.hostname}`}
              onClick={onDelete}
            >
              <Trash2Icon />
            </Button>
          </>
        }
      >
        <div className="truncate font-mono text-sm">{route.hostname}</div>
        <div className="text-muted-foreground text-sm">
          → Port {route.targetPort}
        </div>
      </DomainRowShell>
      <CertificateEvidence evidence={certificateEvidence} />
    </div>
  );
}
