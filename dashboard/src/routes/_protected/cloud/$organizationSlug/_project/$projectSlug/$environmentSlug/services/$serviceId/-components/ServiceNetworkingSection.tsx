import { useState } from "react";
import { PlusIcon, ZapIcon } from "lucide-react";
import type {
  ServiceManagedHostname,
  ServiceRoute,
} from "#/modules/environment-design/tables";
import { Button } from "#/components/ui/button";
import { Empty, EmptyDescription } from "#/components/ui/empty";
import {
  Field,
  FieldDescription,
  FieldGroup,
  FieldLabel,
} from "#/components/ui/field";
import { SERVICE_DEPLOYMENT_DIFF_PATHS } from "#/modules/services/service-deployment-diff/fields";
import { managedHostname } from "#/modules/environment-design/managed-service-exports";
import { certificateFor, dnsRecordsFor, publicDomainStatus } from "#/modules/services/public-domain-status";
import { usePublicDomainContext } from "#/modules/services/use-public-domain-context";
import type { ServiceDrawerState } from "./useServiceDrawerState";
import { CustomDomainDialog } from "./CustomDomainDialog";
import { DomainTitle, PublicDomainRow } from "./domain-row";
import { ManagedDomainDialog } from "./ManagedDomain";
import { PrivateEndpointField } from "./PrivateEndpointField";

function defaultPrefix(privateDns: string) {
  const label = privateDns
    .toLowerCase()
    .replace(/[^a-z0-9-]/g, "-")
    .replace(/^-+|-+$/g, "");
  return label.length > 0 ? label : "app";
}

function nextFreePrefix(base: string, taken: Iterable<string>) {
  const used = new Set(taken);
  let candidate = base;
  for (let n = 2; used.has(candidate); n += 1) candidate = `${base}-${n}`;
  return candidate;
}

type PublicDomainEditor =
  | { kind: "managed"; index: number }
  | { kind: "generate" }
  | { kind: "route"; index: number }
  | { kind: "add" }
  | null;

export function ServiceNetworkingSection({
  state,
}: {
  state: ServiceDrawerState;
}) {
  const { service, collection, diff, managedPrefixesInUse, defaultTargetPort } =
    state;
  const routesDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.routes);
  const managedDiff = diff.field(
    SERVICE_DEPLOYMENT_DIFF_PATHS.managedHostnames
  );
  const routes = service.routes;
  const managedList = service.managedHostnames;
  const { clusterDomain, clusterStatus, ingressAddresses, observed, certificates } =
    usePublicDomainContext(state.organizationSlug);
  const [editor, setEditor] = useState<PublicDomainEditor>(null);
  const portLabel = (targetPort: number | null) => {
    const port = targetPort ?? defaultTargetPort;
    return port === null ? "Uses PORT" : `Port ${port}`;
  };

  // Optimistic: the service writer rolls back and toasts if saving fails.
  function commitRoutes(next: ServiceRoute[]) {
    collection.update(service.id, (draft) => {
      draft.routes = next;
    });
  }

  function commitManaged(next: ServiceManagedHostname[]) {
    collection.update(service.id, (draft) => {
      draft.managedHostnames = next;
    });
  }

  const takenPrefixesFor = (index: number | null) => [
    ...managedPrefixesInUse,
    ...managedList
      .filter((_, current) => current !== index)
      .map((managed) => managed.prefix),
  ];
  const editedManaged =
    editor?.kind === "managed" ? managedList[editor.index] : undefined;
  const editedRoute =
    editor?.kind === "route" ? routes[editor.index] : undefined;

  return (
    <FieldGroup>
      <Field
        data-changed={managedDiff.changed || routesDiff.changed || undefined}
      >
        <FieldLabel>Public Networking</FieldLabel>
        <FieldDescription>
          Access your application over HTTP with the following domains.
        </FieldDescription>
        <div className="flex flex-col gap-2">
          {managedList.length === 0 && routes.length === 0 ? (
            <Empty>
              <EmptyDescription>No public domains yet.</EmptyDescription>
            </Empty>
          ) : null}
          {managedList.map((managed, index) => {
            const hostname = clusterDomain ? managedHostname(managed.prefix, clusterDomain) : null;
            const status = publicDomainStatus({
              domain: { kind: "generated" },
              deployed: state.appliedDomains.managedPrefixes.has(managed.prefix),
              observed,
              clusterDomain: clusterStatus,
            });
            return (
              <PublicDomainRow
                key={managed.prefix}
                organizationSlug={state.organizationSlug}
                title={
                  hostname ? (
                    <DomainTitle hostname={hostname} live={status.kind === "live"} />
                  ) : (
                    <div className="truncate font-mono text-sm">
                      {managed.prefix}
                      <span className="text-muted-foreground">.…</span>
                    </div>
                  )
                }
                label={hostname ?? managed.prefix}
                portLabel={portLabel(managed.targetPort)}
                status={status}
                changed={managedDiff.changed}
                onEdit={() => setEditor({ kind: "managed", index })}
                onDelete={() => commitManaged(managedList.filter((_, current) => current !== index))}
              />
            );
          })}
          {routes.map((route, index) => {
            const status = publicDomainStatus({
              domain: { kind: "custom", certificate: certificateFor(route.hostname, certificates) },
              deployed: state.appliedDomains.routeHostnames.has(route.hostname),
              observed,
              clusterDomain: clusterStatus,
            });
            return (
              <PublicDomainRow
                key={route.id}
                organizationSlug={state.organizationSlug}
                title={
                  <DomainTitle hostname={route.hostname} live={status.kind === "live"} />
                }
                label={route.hostname}
                portLabel={portLabel(route.targetPort)}
                status={status}
                dnsRecords={dnsRecordsFor(route.hostname, clusterDomain, ingressAddresses)}
                changed={routesDiff.changed}
                onEdit={() => setEditor({ kind: "route", index })}
                onDelete={() => commitRoutes(routes.filter((_, current) => current !== index))}
              />
            );
          })}
        </div>
        <div className="flex flex-wrap gap-2">
          <Button
            type="button"
            variant="outline"
            onClick={() => setEditor({ kind: "generate" })}
          >
            <ZapIcon data-icon="inline-start" />
            Generate Domain
          </Button>
          <Button
            type="button"
            variant="outline"
            onClick={() => setEditor({ kind: "add" })}
          >
            <PlusIcon data-icon="inline-start" />
            Custom Domain
          </Button>
        </div>

        {editor?.kind === "generate" ? (
          <ManagedDomainDialog
            mode="generate"
            managed={{
              prefix: nextFreePrefix(
                defaultPrefix(service.privateDns),
                takenPrefixesFor(null)
              ),
              targetPort: null,
            }}
            clusterDomain={clusterDomain}
            takenPrefixes={takenPrefixesFor(null)}
            defaultTargetPort={defaultTargetPort}
            onClose={() => setEditor(null)}
            onSubmit={(next) => commitManaged([...managedList, next])}
          />
        ) : null}
        {editor?.kind === "managed" && editedManaged ? (
          <ManagedDomainDialog
            managed={editedManaged}
            clusterDomain={clusterDomain}
            takenPrefixes={takenPrefixesFor(editor.index)}
            defaultTargetPort={defaultTargetPort}
            onClose={() => setEditor(null)}
            onSubmit={(next) =>
              commitManaged(
                managedList.map((managed, index) =>
                  index === editor.index ? next : managed
                )
              )
            }
          />
        ) : null}
        {editor?.kind === "add" ? (
          <CustomDomainDialog
            defaultTargetPort={defaultTargetPort}
            onClose={() => setEditor(null)}
            onSubmit={(next) => commitRoutes([...routes, next])}
          />
        ) : null}
        {editor?.kind === "route" && editedRoute ? (
          <CustomDomainDialog
            route={editedRoute}
            defaultTargetPort={defaultTargetPort}
            onClose={() => setEditor(null)}
            onSubmit={(next) =>
              commitRoutes(
                routes.map((route, index) =>
                  index === editor.index ? next : route
                )
              )
            }
          />
        ) : null}
      </Field>
      <PrivateEndpointField state={state} />
    </FieldGroup>
  );
}
