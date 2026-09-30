import type { RuntimeServiceRecord } from "#/modules/runtime/runtime.collection";

/** Where Ployz runs its own Services on each Server (core's `Namespace::SYSTEM`). */
export const SYSTEM_NAMESPACE = "ployz-system";

/**
 * Each Service with a container on some Server, named as the Engine names it: `namespace/name`. Hook containers are
 * one-off jobs and do not count, and neither does Ployz's own ingress and DNS (`ployz-system`): nothing the user runs.
 */
export function servicesOnServers(runtime: readonly Pick<RuntimeServiceRecord, "identity" | "containers">[]) {
  return runtime.flatMap(({ identity, containers }) => {
    const slash = identity.indexOf("/");
    if (containers.length === 0 || (slash >= 0 && identity.slice(0, slash) === SYSTEM_NAMESPACE)) return [];
    return [{
      identity,
      name: slash < 0 ? identity : identity.slice(slash + 1),
      /** The Namespace it runs in; null when the Engine names none. */
      namespace: slash < 0 ? null : identity.slice(0, slash),
      machineIds: new Set(containers.map((container) => container.machineId)),
    }];
  });
}
