import type { RuntimeServiceRecord } from "#/modules/runtime/runtime.collection";

/** Where Ployz runs its own Services on each Server (core's `Namespace::SYSTEM`). */
export const SYSTEM_NAMESPACE = "ployz-system";

/** A Qualified Service (`namespace/name`) split; `namespace` is null when the Engine names none. */
export function splitQualifiedService(service: string) {
  const slash = service.indexOf("/");
  return slash < 0
    ? { namespace: null, name: service }
    : { namespace: service.slice(0, slash), name: service.slice(slash + 1) };
}

/**
 * Each Service with a container on some Server, named as the Engine names it: `namespace/name`. Hook containers are
 * one-off jobs and do not count, and neither does Ployz's own ingress and DNS (`ployz-system`): nothing the user runs.
 */
export function servicesOnServers(runtime: readonly Pick<RuntimeServiceRecord, "identity" | "containers">[]) {
  return runtime.flatMap(({ identity, containers }) => {
    const { namespace, name } = splitQualifiedService(identity);
    if (containers.length === 0 || namespace === SYSTEM_NAMESPACE) return [];
    return [{ identity, name, namespace, machineIds: new Set(containers.map((container) => container.machineId)) }];
  });
}
