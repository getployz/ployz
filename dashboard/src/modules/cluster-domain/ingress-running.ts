import type { RuntimeWatchView } from "@ployz/sdk";

const INGRESS = "ployz-system/ingress";

const AT_REST = new Set(["created", "exited", "dead"]);

function ingressOn(frame: Pick<RuntimeWatchView, "services">, counts: (state: string) => boolean): Set<string> {
  const ingress = frame.services.find((service) => service.identity === INGRESS);
  return new Set(ingress?.containers.flatMap((container) => counts(container.runtime.state) ? [container.machine_id] : []));
}

/** The Machines, by id, where the Ingress Proxy's container is running. */
export function ingressRunningOn(frame: Pick<RuntimeWatchView, "services">): Set<string> {
  return ingressOn(frame, (state) => state === "running");
}

/** The Machines, by id, with an Ingress Proxy container a Deploy would leave alone: any not created, exited or dead. */
export function ingressAwakeOn(frame: Pick<RuntimeWatchView, "services">): Set<string> {
  return ingressOn(frame, (state) => !AT_REST.has(state));
}

/** Passes the frames through, and calls `changed` each time the Machines running the Ingress Proxy differ from the frame before. */
export async function* onIngressChange<Frame extends Pick<RuntimeWatchView, "services">>(
  frames: AsyncIterable<Frame>,
  changed: () => void,
): AsyncGenerator<Frame> {
  let before: string | undefined;
  for await (const frame of frames) {
    const running = [...ingressRunningOn(frame)].sort().join(",");
    if (before !== undefined && running !== before) changed();
    before = running;
    yield frame;
  }
}
