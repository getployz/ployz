import type { RuntimeWatchView, ServiceObservation } from "@ployz/sdk";
import { FOURS, NORMAL, ONES, TWOS } from "#/modules/metrics/metrics.verify-fixture";
import { requirements } from "#/modules/runtime/daemon-compat";
import {
  resolvedServiceSpecFixture,
  runtimeWatchContainerFixture,
  runtimeWatchFrameFixture,
  runtimeWatchMachineFixture,
  runtimeWatchMachineObservationFixture,
} from "#/modules/runtime/runtime-watch-frame.test-fixture";

const machine = (id: string, name: string, membership: string, daemonVersion: string, capabilities: string[]) =>
  runtimeWatchMachineObservationFixture({
    membership,
    storage: { state: "pool", size_bytes: 400e9, used_bytes: 120e9, free_bytes: 280e9 },
    machine: runtimeWatchMachineFixture(id, name, {
      runtime: { ...runtimeWatchMachineFixture(id, name).runtime, daemon_version: daemonVersion, capabilities },
    }),
  });

const NAMESPACE = "shop-production";

/** Named like the metrics fixture's Services, so each row on a Server page reads that Service's canned history. */
const service = (name: string, machineIds: readonly string[]): ServiceObservation => {
  const containers = machineIds.map((machineId, index) => {
    const container = runtimeWatchContainerFixture(machineId, `${name}-${index + 1}`);
    return { ...container, namespace: NAMESPACE, resolved_spec: { ...container.resolved_spec, name } };
  });
  return { identity: `${NAMESPACE}/${name}`, service_id: resolvedServiceSpecFixture().service_id, containers, hook_containers: [] };
};

export const verifyRuntimeFrame = (): RuntimeWatchView => {
  const services = [
    service("api", [NORMAL, TWOS]),
    service("cron", [NORMAL]),
    service("postgres", [NORMAL, ONES]),
    service("redis", [NORMAL]),
    service("web", [NORMAL, ONES]),
    service("worker", [NORMAL]),
  ];
  return runtimeWatchFrameFixture({
    machines: [
      machine(NORMAL, "fra-1", "up", "0.3.0", [requirements.metrics.capability]),
      machine(ONES, "fra-2", "down", "0.3.0", [requirements.metrics.capability]),
      machine(TWOS, "syd-1", "up", "0.2.4", [requirements.liveLogs.capability]),
      machine(FOURS, "ams-1", "up", "0.3.0", [requirements.metrics.capability]),
    ],
    services,
    containers: services.flatMap((entry) => entry.containers),
    observed_at: new Date().toISOString(),
  });
};

export async function* verifyRuntimeFrameHeldOpen(signal: AbortSignal): AsyncIterable<RuntimeWatchView> {
  yield verifyRuntimeFrame();
  await new Promise((resolve) => signal.addEventListener("abort", resolve, { once: true }));
}
