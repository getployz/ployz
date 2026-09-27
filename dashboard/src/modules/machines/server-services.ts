import type { RuntimeServiceRecord } from "#/modules/runtime/runtime.collection";

/** The Cloud fields that name a Service the way the Engine does: `namespace/privateDns`. */
type CloudService = { name: string; privateDns: string; environmentSlug: string };

export type ServiceOnServers<S extends CloudService> = {
  identity: string;
  name: string;
  /** Null when the Service was not deployed from this Organization's Cloud. */
  cloud: S | null;
  machineIds: ReadonlySet<string>;
};

/** Each Service with a container on some Server, joined to its Cloud Service. Hook containers are one-off jobs and do not count. */
export function servicesOnServers<S extends CloudService>(
  runtime: readonly Pick<RuntimeServiceRecord, "identity" | "containers">[],
  cloud: readonly S[],
): ServiceOnServers<S>[] {
  const byIdentity = new Map(cloud.map((service) => [`${service.environmentSlug}/${service.privateDns}`, service]));
  return runtime.flatMap(({ identity, containers }) => {
    if (containers.length === 0) return [];
    const match = byIdentity.get(identity) ?? null;
    return [{
      identity,
      name: match?.name ?? identity.slice(identity.indexOf("/") + 1),
      cloud: match,
      machineIds: new Set(containers.map((container) => container.machineId)),
    }];
  });
}
