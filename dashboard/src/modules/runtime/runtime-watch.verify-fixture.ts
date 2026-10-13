import type { RuntimeWatchView } from "@ployz/sdk";
import { NORMAL, ONES, TWOS } from "#/modules/metrics/metrics.verify-fixture";
import { requirements } from "#/modules/runtime/daemon-compat";
import {
  runtimeWatchFrameFixture,
  runtimeWatchMachineFixture,
  runtimeWatchMachineObservationFixture,
} from "#/modules/runtime/runtime-watch-frame.test-fixture";

const machine = (id: string, name: string, membership: string, daemonVersion: string, capabilities: string[]) =>
  runtimeWatchMachineObservationFixture({
    membership,
    machine: runtimeWatchMachineFixture(id, name, {
      runtime: { ...runtimeWatchMachineFixture(id, name).runtime, daemon_version: daemonVersion, capabilities },
    }),
  });

export const verifyRuntimeFrame = (): RuntimeWatchView => runtimeWatchFrameFixture({
  machines: [
    machine(NORMAL, "fra-1", "up", "0.3.0", [requirements.metrics.capability]),
    machine(ONES, "fra-2", "down", "0.3.0", [requirements.metrics.capability]),
    machine(TWOS, "syd-1", "up", "0.2.4", [requirements.liveLogs.capability]),
  ],
  observed_at: new Date().toISOString(),
});

export async function* verifyRuntimeFrameHeldOpen(signal: AbortSignal): AsyncIterable<RuntimeWatchView> {
  yield verifyRuntimeFrame();
  await new Promise((resolve) => signal.addEventListener("abort", resolve, { once: true }));
}
