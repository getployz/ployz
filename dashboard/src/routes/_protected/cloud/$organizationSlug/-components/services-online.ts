import { useLiveQuery } from "@tanstack/react-db";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { getRuntimeCollections, isIncompleteObservation, type RuntimeLensStatus, type RuntimeServiceRecord } from "#/modules/runtime/runtime.collection";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { useRuntimeStatus } from "#/providers/runtime-provider";

/** Runtime evidence for services-online summaries; incomplete evidence counts as unavailable. */
export function useRuntimeServices(organizationSlug: string) {
  const scope = useCollectionScope();
  const { data: runtimeServices } = useLiveQuery(getRuntimeCollections(organizationSlug, scope).services);
  const { lensStatus, incompleteIds } = useRuntimeStatus();
  const { offline } = useRuntimeLens(organizationSlug);
  const runtimeStatus: RuntimeLensStatus = isIncompleteObservation(incompleteIds) ? "unavailable" : lensStatus;
  return { runtimeServices, runtimeStatus, offline };
}

/** A container serves when it runs and is healthy, has no health check, or passed its check once and is failing it now. */
export const containerServing = (container: RuntimeServiceRecord["containers"][number]) => container.runtime?.state === "running" &&
  (container.runtime.health === "healthy" || container.runtime.health === "not_configured" || container.runtime.health === "failing");

/** A Service is online when one of its containers serves on a Server that is not `offline`. */
const serviceOnline = (runtime: Pick<RuntimeServiceRecord, "containers">, offline: ReadonlySet<string>) =>
  runtime.containers.some((container) => !offline.has(container.machineId) && containerServing(container));

/**
 * `2/3 services online`: a Service counts once when a running container serves; hooks never count.
 * `online` is null when runtime evidence is not observed, and the label then shows only the service count.
 */
export function servicesOnline(
  environment: { namespace: string; services: readonly { slug: string }[] },
  runtimeServices: readonly RuntimeServiceRecord[],
  runtimeStatus: RuntimeLensStatus,
  offline: ReadonlySet<string>,
) {
  const serviceCount = environment.services.length;
  const online = runtimeStatus === "observed"
    ? environment.services.filter(service => runtimeServices.some(runtime =>
      runtime.identity === `${environment.namespace}/${service.slug}` && serviceOnline(runtime, offline))).length
    : null;
  const noun = serviceCount === 1 ? "service" : "services";
  const label = serviceCount === 0 ? "No services"
    : online === null ? `${serviceCount} ${noun}` : `${online}/${serviceCount} ${noun} online`;
  return { online, label };
}
