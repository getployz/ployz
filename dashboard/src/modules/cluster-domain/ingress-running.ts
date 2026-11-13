import type { RuntimeWatchView } from "@ployz/sdk";

/** The Ingress Proxy's Service identity. */
export const INGRESS = "ployz-system/ingress";

/** The Tunnel Connector's Service identity: the Cloudflare tunnel's connector on a Server. */
export const TUNNEL = "ployz-system/tunnel";

/** The Public Door's holder: the Service that owns host ports 80 and 443 on a Server. */
export const INGRESS_HOLDER = "ployz-system/ingress-holder";

const PUBLIC_PORTS = new Set([80, 443]);

/**
 * Whether any holder or proxy container is specified to publish host port 80 or 443, running or not: one restarting,
 * just created or stopped still owns the ports the moment it runs.
 */
export function publicSpecifiedAnywhere(frame: Pick<RuntimeWatchView, "services">): boolean {
  return frame.services.some((service) => (service.identity === INGRESS || service.identity === INGRESS_HOLDER)
    && service.containers.some((container) => container.resolved_spec.ports.some((port) => port.mode === "host" && PUBLIC_PORTS.has(port.published_port))));
}

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

/**
 * The Doors each Machine holds open, by id, read as the CLI reads them from the running door containers: a holder or
 * proxy publishing host port 80 or 443 opens the Public Door, a running connector the Tunnel Door. A Machine with
 * neither running is absent.
 */
export function doorsOpenOn(frame: Pick<RuntimeWatchView, "services">): Map<string, Set<"public" | "tunnel">> {
  const doors = new Map<string, Set<"public" | "tunnel">>();
  for (const service of frame.services) {
    if (service.identity !== INGRESS && service.identity !== INGRESS_HOLDER && service.identity !== TUNNEL) continue;
    for (const container of service.containers) {
      if (container.runtime.state !== "running") continue;
      const publishes = container.resolved_spec.ports.some((port) => port.mode === "host" && PUBLIC_PORTS.has(port.published_port));
      const door = service.identity === TUNNEL ? "tunnel" : publishes ? "public" : null;
      if (door === null) continue;
      const held = doors.get(container.machine_id) ?? new Set();
      held.add(door);
      doors.set(container.machine_id, held);
    }
  }
  return doors;
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
