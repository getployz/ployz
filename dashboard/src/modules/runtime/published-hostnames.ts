import type { PublishedHostname, RuntimeWatchView } from "@ployz/sdk";

/**
 * The hostnames the Servers publish now, each with the Namespace and Service publishing it. As in the runtime's
 * `hostname_owners`, only Service containers claim one: a pre-deploy hook keeps its spec's ports but serves nothing.
 */
export function publishedOf(frame: RuntimeWatchView): PublishedHostname[] {
  const seen = new Map<string, PublishedHostname>();
  for (const container of frame.containers) {
    if (container.kind !== "service_container") continue;
    for (const port of container.resolved_spec.ports) {
      if (port.mode === "ingress") seen.set(`${container.namespace}/${port.hostname}`, { hostname: port.hostname, namespace: container.namespace, service: container.resolved_spec.name });
    }
  }
  return [...seen.values()];
}
