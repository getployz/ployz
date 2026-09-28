import { runtimeServiceIdentity, type RuntimeServiceRecord } from "#/modules/runtime/runtime.collection";

/**
 * Each Service with a container on some Server, joined to its Cloud Service; `cloud` is null for a Service
 * this Organization's Cloud did not deploy. Hook containers are one-off jobs and do not count.
 */
export function servicesOnServers<S extends { name: string; privateDns: string; environmentSlug: string }>(
  runtime: readonly Pick<RuntimeServiceRecord, "identity" | "containers">[],
  cloud: readonly S[],
) {
  const byIdentity = new Map(cloud.map((service) => [runtimeServiceIdentity(service), service]));
  return runtime.flatMap(({ identity, containers }) => {
    if (containers.length === 0) return [];
    const match = byIdentity.get(identity) ?? null;
    return [{
      identity,
      name: match?.name ?? identity,
      cloud: match,
      machineIds: new Set(containers.map((container) => container.machineId)),
    }];
  });
}
