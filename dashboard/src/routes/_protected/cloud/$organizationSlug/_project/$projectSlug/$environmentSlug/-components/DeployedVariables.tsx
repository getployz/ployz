import { useState } from "react";
import { eq, useLiveSuspenseQuery } from "@tanstack/react-db";
import { useQuery } from "@tanstack/react-query";
import type { ServiceConfig } from "@ployz/sdk/config";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { getRawServicesCollection } from "#/collections/collections";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "#/components/ui/collapsible";
import { deploymentServiceVariablesQueryOptions } from "#/modules/deployments/deployment-variables.queries";
import { DeployedVariableValue } from "./DeployedVariableValue";

/** The attempt's variables for one service, resolved as deployed; read when opened. */
export function DeployedVariables({ organizationSlug, deploymentId, environmentId, serviceId, env }: {
  organizationSlug: string;
  deploymentId: string;
  environmentId: string;
  serviceId: string;
  env: ServiceConfig["env"];
}) {
  const [open, setOpen] = useState(false);
  const values = useQuery({ ...deploymentServiceVariablesQueryOptions(organizationSlug, deploymentId, serviceId), enabled: open });
  const services = getRawServicesCollection(organizationSlug, useCollectionScope());
  const { data: named } = useLiveSuspenseQuery({
    queryKey: ["deployed-variables-services", services.id, environmentId],
    query: (q) => q.from({ service: services }).where(({ service }) => eq(service.environmentId, environmentId))
      .select(({ service }) => ({ id: service.id, lineageId: service.lineageId, name: service.name })),
  });
  // Authored keys plus what deploy added (PLOYZ_PUBLIC_DOMAIN).
  const keys = [...new Set([...Object.keys(env), ...Object.keys(values.data ?? {})])].sort((a, b) => a.localeCompare(b));
  // The services a template reads from. The frozen producers still resolve a service deleted since; only its name is gone.
  const sources = (value: ServiceConfig["env"][string] | undefined) => value?.kind === "literal" && value.parts
    ? [...new Set(value.parts.flatMap((part) => part.kind !== "ref" ? [] : [named.find((service) =>
      part.owner.scope === "self" ? service.id === serviceId : service.lineageId === part.owner.lineageId)?.name ?? "a deleted service"]))]
    : [];

  return (
    <Collapsible open={open} onOpenChange={setOpen}>
      <CollapsibleTrigger className="cursor-pointer font-medium">
        {keys.length} {keys.length === 1 ? "variable" : "variables"} (as deployed)
      </CollapsibleTrigger>
      <CollapsibleContent>
        {values.isError ? <p className="mt-2 text-destructive">{values.error.message}</p> : (
          <dl className="mt-2 grid grid-cols-[auto_1fr] items-center gap-x-4 gap-y-1 font-mono text-xs">
            {keys.flatMap((key) => [
              <dt key={`${key}:dt`}>{key}</dt>,
              <dd key={`${key}:dd`} className="min-w-0">
                <DeployedVariableValue name={key} loading={values.data === undefined} value={values.data?.[key]} from={sources(env[key])} />
              </dd>,
            ])}
          </dl>
        )}
      </CollapsibleContent>
    </Collapsible>
  );
}
