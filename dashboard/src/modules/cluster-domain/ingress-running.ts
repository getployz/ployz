import type { RuntimeWatchView } from "@ployz/sdk";

/** The Ingress Proxy's Service identity. */
export const INGRESS = "ployz-system/ingress";

/** The Tunnel Connector's Service identity: the Cloudflare tunnel's connector on a Server. */
export const TUNNEL = "ployz-system/tunnel";

const AT_REST = new Set(["created", "exited", "dead"]);

function ingressOn(frame: Pick<RuntimeWatchView, "services">, counts: (state: string) => boolean, identity = INGRESS): Set<string> {
  const ingress = frame.services.find((service) => service.identity === identity);
  return new Set(ingress?.containers.flatMap((container) => counts(container.runtime.state) ? [container.machine_id] : []));
}

/** The Machines, by id, where the Ingress Proxy's container is running. */
export function ingressRunningOn(frame: Pick<RuntimeWatchView, "services">): Set<string> {
  return ingressOn(frame, (state) => state === "running");
}

/** The Machines, by id, where the Tunnel Connector's container is running. */
export function tunnelRunningOn(frame: Pick<RuntimeWatchView, "services">): Set<string> {
  return ingressOn(frame, (state) => state === "running", TUNNEL);
}

/** The Machines, by id, with an Ingress Proxy container a Deploy would leave alone: any not created, exited or dead. */
export function ingressAwakeOn(frame: Pick<RuntimeWatchView, "services">): Set<string> {
  return ingressOn(frame, (state) => !AT_REST.has(state));
}

/** Passes the frames through, and calls `changed` each time the Machines running, or awake with, the Ingress Proxy differ from the frame before. */
export async function* onIngressChange<Frame extends Pick<RuntimeWatchView, "services">>(
  frames: AsyncIterable<Frame>,
  changed: () => void,
): AsyncGenerator<Frame> {
  let before: string | undefined;
  for await (const frame of frames) {
    const now = `${[...ingressRunningOn(frame)].sort().join(",")}|${[...ingressAwakeOn(frame)].sort().join(",")}`;
    if (before !== undefined && now !== before) changed();
    before = now;
    yield frame;
  }
}
