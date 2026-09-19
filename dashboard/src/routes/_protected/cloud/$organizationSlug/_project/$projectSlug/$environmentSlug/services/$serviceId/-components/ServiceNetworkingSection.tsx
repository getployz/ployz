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
import { useRuntimeStatus } from "#/providers/runtime-provider";
import type { ServiceDrawerState } from "./useServiceDrawerState";
import { CustomDomainDialog } from "./CustomDomainDialog";
import { CustomDomainRow, type DomainCertificateEvidence } from "./domain-row";
import { ManagedDomainDialog, ManagedDomainRow } from "./ManagedDomain";
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
  const runtimeStatus = useRuntimeStatus();
  const hostedDnsHostname = runtimeStatus.hostedDnsHostname;
  const hostedDnsHostnameIsCurrent = runtimeStatus.lensStatus === "observed";
  const [editor, setEditor] = useState<PublicDomainEditor>(null);

  const certificateEvidence = (hostname: string): DomainCertificateEvidence => {
    const certificate = runtimeStatus.certificates.find(
      (candidate) => candidate.hostname === hostname
    );
    const incomplete =
      runtimeStatus.incompleteIds.certificates.includes(hostname);
    if (!certificate && !incomplete) return null;
    return {
      status: certificate?.status ?? null,
      lastObserved: !hostedDnsHostnameIsCurrent,
      incomplete,
    };
  };

  async function commitRoutes(next: ServiceRoute[]) {
    await collection.update(service.id, (draft) => {
      draft.routes = next;
    }).isPersisted.promise;
  }

  async function commitManaged(next: ServiceManagedHostname[]) {
    await collection.update(service.id, (draft) => {
      draft.managedHostnames = next;
    }).isPersisted.promise;
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
          {managedList.map((managed, index) => (
            <ManagedDomainRow
              key={managed.prefix}
              managed={managed}
              hostedDnsHostname={hostedDnsHostname}
              hostedDnsHostnameIsCurrent={hostedDnsHostnameIsCurrent}
              certificateEvidence={
                hostedDnsHostname
                  ? certificateEvidence(
                      `${managed.prefix}.${hostedDnsHostname}`
                    )
                  : null
              }
              defaultTargetPort={defaultTargetPort}
              changed={managedDiff.changed}
              onEdit={() => setEditor({ kind: "managed", index })}
              onDelete={() =>
                void commitManaged(
                  managedList.filter((_, current) => current !== index)
                )
              }
            />
          ))}
          {routes.map((route, index) => (
            <CustomDomainRow
              key={route.id}
              route={route}
              defaultTargetPort={defaultTargetPort}
              certificateEvidence={certificateEvidence(route.hostname)}
              changed={routesDiff.changed}
              onEdit={() => setEditor({ kind: "route", index })}
              onDelete={() =>
                void commitRoutes(
                  routes.filter((_, current) => current !== index)
                )
              }
            />
          ))}
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
            hostedDnsHostname={hostedDnsHostname}
            takenPrefixes={takenPrefixesFor(null)}
            defaultTargetPort={defaultTargetPort}
            onClose={() => setEditor(null)}
            onSubmit={(next) => commitManaged([...managedList, next])}
          />
        ) : null}
        {editor?.kind === "managed" && editedManaged ? (
          <ManagedDomainDialog
            managed={editedManaged}
            hostedDnsHostname={hostedDnsHostname}
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
