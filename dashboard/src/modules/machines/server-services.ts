import type { RuntimeServiceRecord } from "#/modules/runtime/runtime.collection";

/**
 * Each Service with a container on some Server, named as the Engine names it: `namespace/name`. Hook containers are
 * one-off jobs and do not count.
 */
export function servicesOnServers(runtime: readonly Pick<RuntimeServiceRecord, "identity" | "containers">[]) {
  return runtime.flatMap(({ identity, containers }) => {
    if (containers.length === 0) return [];
    const slash = identity.indexOf("/");
    return [{
      identity,
      name: slash < 0 ? identity : identity.slice(slash + 1),
      /** The Namespace it runs in; null when the Engine names none. */
      namespace: slash < 0 ? null : identity.slice(0, slash),
      machineIds: new Set(containers.map((container) => container.machineId)),
    }];
  });
}
