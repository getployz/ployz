import { useState, type ReactNode } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import {
  CircleCheckIcon,
  CircleAlertIcon,
  ChevronDownIcon,
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
import type { RuntimeRouteBinding } from "#/modules/runtime/runtime.collection";
import { Badge } from "#/components/ui/badge";
import {
  Alert,
  AlertAction,
  AlertDescription,
  AlertTitle,
} from "#/components/ui/alert";
import { Button } from "#/components/ui/button";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "#/components/ui/collapsible";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import {
  Field,
  FieldDescription,
  FieldGroup,
  FieldLabel,
} from "#/components/ui/field";
import { Spinner } from "#/components/ui/spinner";
import {
  Item,
  ItemActions,
  ItemContent,
  ItemDescription,
  ItemGroup,
  ItemTitle,
} from "#/components/ui/item";
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
import {
  useRuntimeMachines,
  useRuntimePublicUrl,
  useRuntimeService,
  useRuntimeStatus,
} from "#/providers/runtime-provider";
import {
  customDomainCapabilityPresentation,
  customDomainDnsGuidance,
  managedHostnameRuntimePresentation,
  type ManagedRouteObservation,
  type NetworkingBadgeVariant,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceNetworkingSection.presentation";
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
  const driftRow = diff.field("managedHostname.drift");
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

  const publicUrl = useRuntimePublicUrl();
  const runtimeStatus = useRuntimeStatus();
  const { machines } = useRuntimeMachines();
  const { runtime } = useRuntimeService(
    service.environmentSlug,
    service.privateDns,
  );
  const autoDomain = publicUrl.autoDomain;
  const customDomainGuidance = customDomainDnsGuidance({
    mode: publicUrl.mode,
    leaseApex: publicUrl.leaseApex,
    lensStatus: runtimeStatus.lensStatus,
  });
  const canUseManaged = publicUrl.mode !== "disabled" && autoDomain != null;
  const bindings = runtime?.bindings ?? [];
  const expectedManagedHostname =
    managed != null && autoDomain != null
      ? `${managed.prefix}.${autoDomain}`
      : null;
  const managedRuntime = managedHostnameRuntimePresentation({
    expectedHostname: expectedManagedHostname,
    runtimeStatus: runtimeStatus.lensStatus,
    publicUrl: {
      mode: publicUrl.mode,
      domain: publicUrl.autoDomain,
      leaseApex: publicUrl.leaseApex,
      dnsTarget: publicUrl.dnsTarget,
    },
    bindings,
    machines,
  });
  // TLS status for a custom (user-declared) hostname: verified once its cert is
  // available, pending while it's being issued, or undefined before it's bound.
  function customStatus(hostname: string): "verified" | "pending" | undefined {
    const binding = bindings.find(
      (candidate) =>
        candidate.origin === "declared" && candidate.hostname === hostname,
    );
    if (!binding || binding.tls.status === "unknown") {
      return undefined;
    }
    return binding.tls.status === "available" ? "verified" : "pending";
  }

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
              autoDomain={autoDomain}
              takenPrefixes={managedPrefixesInUse}
              defaultTargetPort={defaultTargetPort}
              changed={managedDiff.changed}
              drift={driftRow.changed ? driftRow : null}
              runtime={managedRuntime}
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
              status={customStatus(route.hostname)}
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
            guidance={customDomainGuidance}
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
            guidance={customDomainGuidance}
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

function ManagedDomainRow({
  managed,
  autoDomain,
  takenPrefixes,
  defaultTargetPort,
  changed,
  drift,
  runtime,
  editing,
  onEdit,
  onCancel,
  onDelete,
  onSubmit,
}: {
  managed: ServiceManagedHostname;
  autoDomain: string | null;
  takenPrefixes: string[];
  defaultTargetPort: number;
  changed: boolean;
  drift: { baselineValue?: string; currentValue?: string } | null;
  runtime: ReturnType<typeof managedHostnameRuntimePresentation>;
  editing: boolean;
  onEdit: () => void;
  onCancel: () => void;
  onDelete: () => void;
  onSubmit: (next: ServiceManagedHostname) => void;
}) {
  const hostname = autoDomain
    ? `${managed.prefix}.${autoDomain}`
    : `${managed.prefix}.…`;
  const port = managed.targetPort ?? defaultTargetPort;

  if (editing) {
    return (
      <ManagedDomainForm
        managed={managed}
        autoDomain={autoDomain}
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
            <Badge variant={runtime.route.variant}>
              {runtime.route.variant === "secondary" ? <Spinner /> : null}
              {runtime.route.label}
            </Badge>
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
      <ManagedDomainRuntimeEvidence runtime={runtime} />
      {drift ? (
        <FieldDescription className="text-changed">
          Cluster domain changed. Redeploy to move to {drift.currentValue}.
        </FieldDescription>
      ) : null}
    </div>
  );
}

function ManagedDomainRuntimeEvidence({
  runtime,
}: {
  runtime: ReturnType<typeof managedHostnameRuntimePresentation>;
}) {
  const healthy = [
    runtime.route.variant,
    runtime.tls.variant,
    runtime.dns.variant,
    ...runtime.gateways.map((gateway) => gateway.variant),
  ].every((variant) => variant === "success");

  if (runtime.freshness.status === "current" && healthy) {
    return (
      <Collapsible>
        <CollapsibleTrigger
          render={<Button type="button" variant="ghost" size="sm" />}
        >
          <CircleCheckIcon data-icon="inline-start" />
          Runtime healthy
          <ChevronDownIcon data-icon="inline-end" />
        </CollapsibleTrigger>
        <CollapsibleContent>
          <div className="pt-2">
            <RuntimeEvidenceList runtime={runtime} />
          </div>
        </CollapsibleContent>
      </Collapsible>
    );
  }

  return (
    <div className="flex flex-col gap-2">
      {runtime.freshness.status === "stale" ? (
        <Alert>
          <CircleAlertIcon />
          <AlertTitle>Runtime evidence is not current</AlertTitle>
          <AlertDescription>
            Route, TLS, DNS, and gateway details are last observed or
            unavailable.
          </AlertDescription>
        </Alert>
      ) : null}
      <RuntimeEvidenceList runtime={runtime} />
    </div>
  );
}

function RuntimeEvidenceList({
  runtime,
}: {
  runtime: ReturnType<typeof managedHostnameRuntimePresentation>;
}) {
  return (
    <ItemGroup>
      <RuntimeEvidenceItem
        title="Route binding"
        label={runtime.route.label}
        variant={runtime.route.variant}
        detail={routeEvidenceDetail(
          runtime.route.observation,
          runtime.freshness.status,
        )}
      />
      <RuntimeEvidenceItem
        title="TLS certificate"
        label={runtime.tls.label}
        variant={runtime.tls.variant}
        detail={tlsEvidenceDetail(
          runtime.route.observation,
          runtime.freshness.status,
        )}
      />
      <RuntimeEvidenceItem
        title="Managed DNS"
        label={runtime.dns.label}
        variant={runtime.dns.variant}
        detail="Allocation and publication are reported independently by Core"
      />
      {runtime.gateways.map((gateway) => (
        <RuntimeEvidenceItem
          key={gateway.id}
          title={gateway.name}
          label={gateway.label}
          variant={gateway.variant}
          detail={gateway.detail}
        />
      ))}
    </ItemGroup>
  );
}

function routeEvidenceDetail(
  observation: ManagedRouteObservation,
  freshness: "current" | "stale",
) {
  switch (observation.status) {
    case "absent":
      return freshness === "current"
        ? "No automatic binding is observed for the desired hostname"
        : "No automatic binding evidence is currently available";
    case "exact":
      return freshness === "current"
        ? `Exact binding ${observation.binding.id}`
        : `Last observed exact binding ${observation.binding.id}`;
    case "prior_serving":
      return freshness === "current"
        ? `Desired route is pending; ${observation.binding.hostname} remains served by binding ${observation.binding.id}`
        : `Desired route was pending; ${observation.binding.hostname} was served by binding ${observation.binding.id}`;
    case "conflict":
      return `${freshness === "current" ? "Conflicting" : "Last observed conflicting"} automatic bindings: ${observation.bindings.map((binding) => binding.id).join(", ")}`;
  }
}

function tlsEvidenceDetail(
  observation: ManagedRouteObservation,
  freshness: "current" | "stale",
) {
  switch (observation.status) {
    case "absent":
      return "TLS state awaits an observed route binding";
    case "conflict":
      return "TLS cannot be attributed while automatic binding identity conflicts";
    case "exact":
      return bindingTlsDetail(observation.binding, "Exact binding", freshness);
    case "prior_serving":
      return bindingTlsDetail(
        observation.binding,
        "Prior serving binding",
        freshness,
      );
  }
}

function bindingTlsDetail(
  binding: RuntimeRouteBinding,
  prefix: string,
  freshness: "current" | "stale",
) {
  const subject =
    freshness === "current" ? prefix : `Last observed ${prefix.toLowerCase()}`;
  switch (binding.tls.status) {
    case "available":
      return `${subject} ${binding.id} ${freshness === "current" ? "uses" : "used"} certificate ${binding.tls.certificateId}`;
    case "unavailable":
      return `${subject} ${binding.id} ${freshness === "current" ? "reports" : "reported"} TLS unavailable`;
    case "unknown":
      return `${subject} ${binding.id} ${freshness === "current" ? "has" : "had"} no TLS evidence`;
  }
}

function RuntimeEvidenceItem({
  title,
  label,
  variant,
  detail,
}: {
  title: string;
  label: string;
  variant: NetworkingBadgeVariant;
  detail: string;
}) {
  return (
    <Item variant="muted" size="xs">
      <ItemContent>
        <ItemTitle>{title}</ItemTitle>
        <ItemDescription>{detail}</ItemDescription>
      </ItemContent>
      <ItemActions>
        <Badge variant={variant}>{label}</Badge>
      </ItemActions>
    </Item>
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
  autoDomain,
  takenPrefixes,
  defaultTargetPort,
  onCancel,
  onSubmit,
}: {
  managed: ServiceManagedHostname;
  autoDomain: string | null;
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
              description={`.${autoDomain ?? "…"}`}
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
  status,
  changed,
  canEdit,
  onEdit,
  onDelete,
}: {
  route: ServiceRoute;
  status: "verified" | "pending" | undefined;
  changed: boolean;
  canEdit: boolean;
  onEdit: () => void;
  onDelete: () => void;
}) {
  return (
    <DomainRowShell
      changed={changed}
      icon={
        status === "verified" ? (
          <CircleCheckIcon className="text-success" />
        ) : (
          <GlobeIcon />
        )
      }
      actions={
        <>
          {status === "verified" ? (
            <Badge variant="success">TLS available</Badge>
          ) : status === "pending" ? (
            <Badge variant="secondary">
              <Spinner />
              TLS pending
            </Badge>
          ) : (
            <Badge variant="outline">TLS not observed</Badge>
          )}
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
  );
}
