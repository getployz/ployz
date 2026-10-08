import "@tanstack/react-start/server-only";
import type { DestructiveEffect, QualifiedService, RuntimeWatchView } from "@ployz/sdk";
import { Effect } from "effect";
import { getApproval, operationDigest, type OperationDigest, pendingApprovals } from "#/modules/approvals/approvals.server";
import type { OperationAsked } from "#/modules/approvals/approvals.server";
import { readStore } from "#/modules/config-store/config-store.server";
import { ownedNamespaces } from "#/modules/machines/namespace-cleanup.server";
import { SYSTEM_NAMESPACE, splitQualifiedService } from "#/modules/machines/server-services";
import type { DataLossIdentity } from "#/modules/runtime/data-loss-identity";
import { firstRuntimeFrame, OrganizationRuntime, RUNTIME_FRAME_TIMEOUT_MS } from "#/modules/runtime/organization-runtime.server";
import { dockerVolumeName } from "#/modules/volume-run/volume-run";
import { Conflict } from "#/server/public-error";

const unreachable = () => new Conflict({ userFacing: true, message: "Your servers aren't answering. Try again once they are." });

const byName = (a: string, b: string) => (a < b ? -1 : a > b ? 1 : 0);
const volumeName = ({ id }: DataLossIdentity) => `${id.machine_id}/${id.name}`;
const deletesVolume = (identity: DataLossIdentity, label = identity.id.name): DestructiveEffect => ({
  kind: "deletes_volume",
  name: label,
  node: volumeName(identity),
  path: `volumes/${volumeName(identity)}`,
});
const removesService = (identity: QualifiedService, serviceId: string): DestructiveEffect => ({
  kind: "removes_service",
  name: identity,
  node: serviceId,
  path: `services/${identity}`,
});
const serverName = (frame: RuntimeWatchView, machineId: string) =>
  frame.machines.find(({ machine }) => machine.id === machineId)?.machine.name;

export function volumeMountLabel(frame: RuntimeWatchView, { id }: DataLossIdentity): string {
  const users = frame.services.flatMap((service) => service.containers.flatMap(({ machine_id, resolved_spec }) => {
    if (machine_id !== id.machine_id) return [];
    const volume = resolved_spec.volumes.find(({ source }) =>
      (source.kind === "ordinary" || source.kind === "provisioned") && source.name === id.name);
    if (volume === undefined) return [];
    const at = resolved_spec.mounts.find((mount) => mount.volume === volume.reference)?.target;
    const name = splitQualifiedService(service.identity).name;
    return [at === undefined ? name : `${name} at ${at}`];
  }));
  const unique = [...new Set(users)].sort(byName);
  const server = serverName(frame, id.machine_id) ?? "a Server Cloud can't see";
  return unique.length === 0 ? `on ${server}` : `used by ${unique.join(", ")} on ${server}`;
}

/** An operation as the gate weighs it, with the services a Drain of it acts on. */
export type DrainPlan = OperationAsked & { readonly targets: QualifiedService[] };

/**
 * What draining `machineId` does to the Services the Organization's Environments own there, from one frame: each moves,
 * stays (a Volume holds it; its data never moves), or retires (a Global slot). A Global with no running slot elsewhere
 * is removed outright. Null when the frame has no such Server.
 */
export function drainPlan(frame: RuntimeWatchView, owned: ReadonlySet<string>, machineId: string): DrainPlan | null {
  const name = serverName(frame, machineId);
  if (name === undefined) return null;
  const moves: QualifiedService[] = [];
  const stays: QualifiedService[] = [];
  const retires: QualifiedService[] = [];
  const effects: DestructiveEffect[] = [];
  for (const service of frame.services) {
    const { namespace } = splitQualifiedService(service.identity);
    if (namespace === null || namespace === SYSTEM_NAMESPACE || !owned.has(namespace)) continue;
    const slots = service.containers.filter((container) => container.kind === "service_container");
    const here = slots.filter((container) => container.machine_id === machineId);
    const newest = here.reduce<(typeof here)[number] | undefined>(
      (latest, container) => latest === undefined || container.created_at_unix_nanos > latest.created_at_unix_nanos ? container : latest,
      undefined,
    );
    if (newest === undefined) continue;
    if (newest.resolved_spec.mode.mode === "global") {
      retires.push(service.identity);
      const runsElsewhere = slots.some((container) => container.machine_id !== machineId && container.runtime.state === "running");
      if (!runsElsewhere) effects.push(removesService(service.identity, service.service_id));
    } else if (newest.resolved_spec.volumes.length > 0) {
      stays.push(service.identity);
    } else {
      moves.push(service.identity);
    }
  }
  [moves, stays, retires].forEach((list) => list.sort(byName));
  effects.sort((a, b) => byName(a.path, b.path));
  return {
    subject: `server:${machineId}`,
    verb: "drain",
    name,
    preview: { server: machineId, moves, stays, retires },
    effects,
    targets: [...moves, ...stays, ...retires].sort(byName),
  };
}

/** What cleaning `namespace` deletes: its Services' containers and the Volumes it holds on each Server. */
export function cleanPlan(
  frame: RuntimeWatchView,
  namespace: string,
  volumes: readonly DataLossIdentity[],
): OperationAsked & { readonly doomed: ReadonlyArray<{ readonly identity: DataLossIdentity; readonly label: string }> } {
  const services = frame.services.filter((service) =>
    splitQualifiedService(service.identity).namespace === namespace
    && service.containers.some((container) => container.kind === "service_container"));
  const running = services.filter((service) =>
    service.containers.some((container) => container.kind === "service_container" && container.runtime.state === "running"));
  const doomed = [...volumes]
    .sort((a, b) => byName(volumeName(a), volumeName(b)))
    .map((identity) => ({ identity, label: volumeMountLabel(frame, identity) }));
  return {
    subject: `namespace:${namespace}`,
    verb: "clean",
    name: namespace,
    preview: {
      namespace,
      services: services.map(({ identity }) => identity).sort(byName),
      volumes: doomed.map(({ identity }) => volumeName(identity)),
    },
    effects: [
      ...running.map((service) => removesService(service.identity, service.service_id)).sort((a, b) => byName(a.path, b.path)),
      ...doomed.map(({ identity, label }) => deletesVolume(identity, label)),
    ],
    doomed,
  };
}

export type VolumeOwner = { readonly dockerName: string; readonly project: string; readonly environment: string; readonly volume: string };

export function volumeLabels(owners: readonly VolumeOwner[]): ReadonlyMap<string, string> {
  const unique = (label: (owner: VolumeOwner) => string, owner: VolumeOwner) => owners.every((other) =>
    label(other) !== label(owner) || (other.project === owner.project && other.environment === owner.environment));
  const bare = (owner: VolumeOwner) => owner.volume;
  const inEnvironment = (owner: VolumeOwner) => `${owner.environment}/${owner.volume}`;
  return new Map(owners.map((owner) => [
    owner.dockerName,
    unique(bare, owner) ? bare(owner)
      : unique(inEnvironment, owner) ? inEnvironment(owner)
      : `${owner.project}/${owner.environment}/${owner.volume}`,
  ]));
}

export function removePlan(
  machineId: string,
  name: string,
  confirmed: readonly DataLossIdentity[] | null,
  labels: ReadonlyMap<string, string> = new Map(),
): OperationAsked {
  const volumes = [...(confirmed ?? [])].sort((a, b) => byName(volumeName(a), volumeName(b)));
  return {
    subject: `server:${machineId}`,
    verb: "remove",
    name,
    preview: { server: machineId, reset: confirmed !== null, volumes: volumes.map(volumeName) },
    effects: [
      { kind: "removes_server", name, node: machineId, path: `servers/${machineId}` },
      ...volumes.map((identity) => deletesVolume(identity, labels.get(identity.id.name))),
    ],
  };
}

const observe = (organizationId: string) => firstRuntimeFrame(organizationId).pipe(
  Effect.mapError(unreachable),
  Effect.flatMap((frame) => frame === null ? Effect.fail(unreachable()) : Effect.succeed(frame)),
);

export const planDrain = Effect.fn("ServerOperations.planDrain")(function* (organizationId: string, machineId: string) {
  const frame = yield* observe(organizationId);
  return drainPlan(frame, new Set(yield* ownedNamespaces(organizationId)), machineId);
});

const ownedOrSystem = (organizationId: string, namespace: string) => namespace === SYSTEM_NAMESPACE
  ? Effect.succeed(true)
  : ownedNamespaces(organizationId).pipe(Effect.map((owned) => owned.includes(namespace)));

/** A clean of `namespace` as it stands now; only of one no Environment owns, and never Ployz's own. */
export const planClean = Effect.fn("ServerOperations.planClean")(function* (organizationId: string, namespace: string) {
  if (yield* ownedOrSystem(organizationId, namespace)) {
    return yield* new Conflict({ userFacing: true, message: `${namespace} belongs to an Environment: delete that Environment instead.` });
  }
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  if (session.status !== "connected") return yield* unreachable();
  const lost = yield* session.connected.dataLossIfNamespaceDestroyed(namespace).pipe(Effect.mapError(unreachable));
  const frame = yield* session.connected.watchFirstFrame(RUNTIME_FRAME_TIMEOUT_MS).pipe(
    Effect.timeout("5 seconds"),
    Effect.mapError(unreachable),
  );
  return cleanPlan(frame, namespace, lost.data_loss);
}, Effect.scoped);

const volumeOwners = Effect.fn("ServerOperations.volumeOwners")(function* (
  organizationId: string,
  doomed: readonly DataLossIdentity[],
) {
  const names = new Set(doomed.map(({ id }) => id.name));
  const owners: VolumeOwner[] = [];
  if (names.size === 0) return owners;
  const { namespaces } = yield* readStore(organizationId, { query: "namespaces" });
  for (const owned of namespaces) {
    if (![...names].some((name) => name.startsWith(`${owned.namespace}_`))) continue;
    const { volumes } = yield* readStore(organizationId, {
      query: "volumes",
      environment: { project: owned.project, environment: owned.environment },
    });
    for (const volume of volumes) {
      const docker = dockerVolumeName(owned.namespace, volume.id);
      if (names.has(docker)) owners.push({ dockerName: docker, project: owned.project, environment: owned.environment, volume: volume.name });
    }
  }
  return owners;
});

export const planRemove = Effect.fn("ServerOperations.planRemove")(function* (
  organizationId: string,
  machineId: string,
  confirmed: readonly DataLossIdentity[] | null,
) {
  const frame = yield* observe(organizationId);
  const name = serverName(frame, machineId);
  if (name === undefined) return null;
  const owned = volumeLabels(yield* volumeOwners(organizationId, confirmed ?? []));
  const labels = new Map((confirmed ?? []).map((identity) =>
    [identity.id.name, owned.get(identity.id.name) ?? volumeMountLabel(frame, identity)]));
  return removePlan(machineId, name, confirmed, labels);
});

/**
 * An operation approval's preview recomputed on read, so a changed Cluster supersedes it. A Server no longer observed,
 * or a Namespace an Environment owns by now, no longer applies. When the Cluster can't be read, the stored preview stands.
 */
export const freshOperationDigest: OperationDigest<Effect.Services<ReturnType<typeof planClean>>> = (organizationId, subject, verb, stored) => {
  const separator = subject.indexOf(":");
  const key = subject.slice(separator + 1);
  const fresh = Effect.gen(function* () {
    switch (verb) {
      case "drain": {
        const frame = yield* observe(organizationId);
        const plan = drainPlan(frame, new Set(yield* ownedNamespaces(organizationId)), key);
        return plan === null ? null : operationDigest(plan);
      }
      case "clean":
        if (yield* ownedOrSystem(organizationId, key)) return null;
        return operationDigest(yield* planClean(organizationId, key));
      case "remove": {
        const frame = yield* observe(organizationId);
        return frame.machines.some(({ machine }) => machine.id === key) ? stored : null;
      }
    }
  });
  return fresh.pipe(Effect.orElseSucceed(() => stored));
};

/** Every approval still waiting in the Organization, each operation's preview recomputed against the Cluster first. */
export const freshPendingApprovals = (organizationId: string) => pendingApprovals(organizationId, freshOperationDigest);

/** One approval in the Organization, its operation's preview recomputed against the Cluster first. */
export const freshApproval = (organizationId: string, id: string) => getApproval(organizationId, id, freshOperationDigest);
