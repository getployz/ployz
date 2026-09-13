import type { RuntimeWatchView } from "@ployz/sdk";
import { Schema } from "effect";
import type { RuntimeSnapshot } from "#/modules/runtime/runtime.collection";

/**
 * The browser needs only enough of the Engine's additive SDK payload to retain
 * useful observation evidence. Extra Engine fields are intentionally ignored
 * here instead of being rebuilt as a second Cloud runtime schema.
 */
const runtimeWatchContainerSchema = Schema.Struct({
  container_id: Schema.String,
  display_name: Schema.String,
  machine_id: Schema.String,
  project_name: Schema.String,
  kind: Schema.String,
});

const runtimeWatchMachineSchema = Schema.Struct({
  machine: Schema.Struct({
    id: Schema.String,
    name: Schema.String,
    public_ip: Schema.NullOr(Schema.String),
    advertised_endpoints: Schema.Array(Schema.String),
  }),
  membership: Schema.String,
});

const runtimeWatchIncompleteVolumeIdSchema = Schema.Struct({
  machine_id: Schema.String,
  name: Schema.String,
});

const runtimeWatchCertificateSchema = Schema.Struct({
  hostname: Schema.String,
  status: Schema.String,
  last_error: Schema.NullOr(Schema.String),
  backoff: Schema.NullOr(
    Schema.Struct({
      failure_kind: Schema.String,
      next_attempt_at: Schema.String,
      failures: Schema.Int.check(Schema.isGreaterThanOrEqualTo(0)),
    }),
  ),
});

const runtimeWatchServiceSchema = Schema.Struct({
  identity: Schema.String,
  service_id: Schema.String,
  containers: Schema.Array(runtimeWatchContainerSchema),
  hook_containers: Schema.Array(runtimeWatchContainerSchema),
});

/** A structural boundary for the wire form of `RuntimeWatchView`. The SDK
 * frame has more data than Cloud retains, but these names and values remain
 * directly constrained by its published wire contract. */
export const runtimeWatchFrameSchema = Schema.Struct({
  services: Schema.Array(runtimeWatchServiceSchema),
  machines: Schema.Array(runtimeWatchMachineSchema),
  containers: Schema.Array(runtimeWatchContainerSchema),
  certificates: Schema.Array(runtimeWatchCertificateSchema),
  hosted_dns_hostname: Schema.NullOr(Schema.String),
  incomplete_ids: Schema.Struct({
    machines: Schema.Array(Schema.String),
    containers: Schema.Array(Schema.String),
    volumes: Schema.Array(runtimeWatchIncompleteVolumeIdSchema),
    certificates: Schema.Array(Schema.String),
  }),
  observed_at: Schema.String,
});

export type RuntimeWatchFrame = typeof runtimeWatchFrameSchema.Type;

/** Connection states carry no Runtime Watch observation. Keeping them separate
 * from `runtime.watch` prevents an empty list from being treated as an
 * authoritative cluster answer. */
export const runtimeConnectionStatusEventSchema = Schema.Struct({
  status: Schema.Literals(["no_connection", "unreachable"]),
  error: Schema.NullOr(Schema.String),
});

export type RuntimeConnectionStatusEvent =
  typeof runtimeConnectionStatusEventSchema.Type;

/**
 * Redact an SDK frame before it crosses the server/browser boundary. Runtime
 * container specs can contain plaintext environment values and configuration;
 * this projection retains only the observation fields Cloud is allowed to
 * display. Its snake_case keys deliberately match the SDK wire payload.
 */
export function runtimeWatchFrameForTransport(
  frame: RuntimeWatchView,
): RuntimeWatchFrame {
  const projected = {
    services: frame.services.map((service) => ({
      identity: service.identity,
      service_id: service.service_id,
      containers: service.containers.map(runtimeWatchContainerForTransport),
      hook_containers: service.hook_containers.map(
        runtimeWatchContainerForTransport,
      ),
    })),
    machines: frame.machines.map((machine) => ({
      machine: {
        id: machine.machine.id,
        name: machine.machine.name,
        public_ip: machine.machine.public_ip,
        advertised_endpoints: [...machine.machine.advertised_endpoints],
      },
      membership: machine.membership,
    })),
    containers: frame.containers.map(runtimeWatchContainerForTransport),
    certificates: frame.certificates.map((certificate) => ({
      hostname: certificate.hostname,
      status: certificate.status,
      last_error: certificate.last_error,
      backoff: certificate.backoff
        ? {
            failure_kind: certificate.backoff.failure_kind,
            next_attempt_at: certificate.backoff.next_attempt_at,
            failures: certificate.backoff.failures,
          }
        : null,
    })),
    hosted_dns_hostname: frame.hosted_dns_hostname,
    incomplete_ids: {
      machines: [...frame.incomplete_ids.machines],
      containers: [...frame.incomplete_ids.containers],
      volumes: frame.incomplete_ids.volumes.map((volume) => ({
        machine_id: volume.machine_id,
        name: volume.name,
      })),
      certificates: [...frame.incomplete_ids.certificates],
    },
    observed_at: frame.observed_at,
  };

  return Schema.decodeUnknownSync(runtimeWatchFrameSchema)(projected, {
    onExcessProperty: "error",
  });
}

/**
 * Map only field spelling and collection keys. The input may be the SDK's
 * typed Runtime Watch frame or the structural browser decode above; neither
 * path invents a deployment, gateway, DNS, revision, or health conclusion.
 */
export function runtimeSnapshotFromWatchFrame(
  frame: RuntimeWatchFrame,
): RuntimeSnapshot {
  const observedAt = frame.observed_at;
  const containerCounts = new Map<string, number>();
  for (const container of frame.containers) {
    containerCounts.set(
      container.machine_id,
      (containerCounts.get(container.machine_id) ?? 0) + 1,
    );
  }

  return {
    status: "observed",
    error: null,
    hostedDnsHostname: frame.hosted_dns_hostname,
    machines: frame.machines.map((machine) => ({
      id: machine.machine.id,
      name: machine.machine.name,
      publicIp: machine.machine.public_ip,
      endpoints: [...machine.machine.advertised_endpoints],
      membership: machine.membership,
      observedContainerCount: containerCounts.get(machine.machine.id) ?? 0,
      observedAt,
    })),
    services: frame.services.map((service) => ({
      id: service.identity,
      identity: service.identity,
      serviceId: service.service_id,
      containers: service.containers.map(runtimeContainerRecordFromWatch),
      hookContainers: service.hook_containers.map(runtimeContainerRecordFromWatch),
      observedAt,
    })),
    certificates: frame.certificates.map((certificate) => ({
      hostname: certificate.hostname,
      status: certificate.status,
      lastError: certificate.last_error,
      backoff: certificate.backoff
        ? {
            failureKind: certificate.backoff.failure_kind,
            nextAttemptAt: certificate.backoff.next_attempt_at,
            failures: certificate.backoff.failures,
          }
        : null,
    })),
    incompleteIds: {
      machines: [...frame.incomplete_ids.machines],
      containers: [...frame.incomplete_ids.containers],
      volumes: frame.incomplete_ids.volumes.map((volume) => ({
        machineId: volume.machine_id,
        name: volume.name,
      })),
      certificates: [...frame.incomplete_ids.certificates],
    },
    observedAt,
  };
}

function runtimeContainerRecordFromWatch(
  container: RuntimeWatchFrame["containers"][number],
) {
  return {
    id: container.container_id,
    displayName: container.display_name,
    machineId: container.machine_id,
    projectName: container.project_name,
    kind: container.kind,
  };
}

function runtimeWatchContainerForTransport(container: {
  container_id: string;
  display_name: string;
  machine_id: string;
  project_name: string;
  kind: string;
}) {
  return {
    container_id: container.container_id,
    display_name: container.display_name,
    machine_id: container.machine_id,
    project_name: container.project_name,
    kind: container.kind,
  };
}
