import type {
  MachineObservation,
  RuntimeWatchView,
} from "@ployz/sdk";
import { Result, Schema } from "effect";
import {
  RUNTIME_PUBLIC_URL_NONE,
  runtimeSnapshotLensSchema,
  type RuntimeMachineRecord,
  type RuntimePublicUrl,
  type RuntimeSnapshotLens,
} from "#/modules/runtime/runtime.collection";
import { Validation } from "#/server/public-error";

export function runtimeSnapshotLensFromWatchFrame(
  frame: RuntimeWatchView,
): Result.Result<RuntimeSnapshotLens, Validation> {
  const decoded = Schema.decodeUnknownResult(runtimeSnapshotLensSchema)(
    projectWatchFrame(frame),
    { onExcessProperty: "error" },
  );
  return Result.isSuccess(decoded)
    ? Result.succeed(decoded.success)
    : Result.fail(
        new Validation({ message: "Runtime watch frame was invalid." }),
      );
}

function projectWatchFrame(frame: RuntimeWatchView): RuntimeSnapshotLens {
  const updatedAt = frame.observed_at;
  const containerCounts = containerCountsByMachine(frame);

  return {
    status: frame.machines.length > 0 ? "live_rows" : "live_empty",
    error: null,
    publicUrl: publicUrlFromWatchFrame(frame),
    machines: frame.machines.map((machine) =>
      machineRecordFromObservation(machine, updatedAt, containerCounts),
    ),
    services: [],
    updatedAt,
  };
}

function publicUrlFromWatchFrame(frame: RuntimeWatchView): RuntimePublicUrl {
  if (frame.hosted_dns_hostname === null) {
    return RUNTIME_PUBLIC_URL_NONE;
  }

  return {
    mode: "ployz",
    domain: frame.hosted_dns_hostname,
    leaseApex: frame.hosted_dns_hostname,
    dnsTarget: {
      intent: "enabled",
      allocation: "allocated",
      publication: "applied",
    },
  };
}

function containerCountsByMachine(frame: RuntimeWatchView) {
  const counts = new Map<string, number>();
  for (const container of frame.containers) {
    counts.set(
      container.machine_id,
      (counts.get(container.machine_id) ?? 0) + 1,
    );
  }
  return counts;
}

function machineRecordFromObservation(
  observation: MachineObservation,
  updatedAt: string,
  containerCounts: Map<string, number>,
): RuntimeMachineRecord {
  const answered = observation.membership === "up";

  return {
    id: observation.machine.id,
    name: observation.machine.name,
    publicIp: observation.machine.public_ip ?? null,
    gateway: { status: "not_installed" },
    observedContainerCount: containerCounts.get(observation.machine.id) ?? 0,
    region: null,
    availabilityZone: null,
    overlayIp: null,
    endpoints: [...observation.machine.advertised_endpoints],
    testimonyStatus: answered ? "answered" : "no_answer",
    lastObservedAt: answered ? updatedAt : null,
    updatedAt,
  };
}
