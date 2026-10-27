import "@tanstack/react-start/server-only";
import type { DestructiveEffect, QualifiedService, RuntimeWatchView } from "@ployz/sdk";
import { Effect } from "effect";
import { getApproval, operationDigest, type OperationDigest, pendingApprovals } from "#/modules/approvals/approvals.server";
import type { OperationAsked } from "#/modules/approvals/approvals.server";
import { readStore } from "#/modules/config-store/config-store.server";
import { unreadable, unreadableEnvironment } from "#/modules/config-store/store-lookup.server";
import { ownedNamespaces } from "#/modules/machines/namespace-cleanup.server";
import { machineUpdateForPolicyChange, type ServerPolicyChange } from "#/modules/machines/server-policy";
import { SYSTEM_NAMESPACE, splitQualifiedService } from "#/modules/machines/server-services";
import { needsAttention, serverStatus } from "#/modules/machines/server-status";
import type { DataLossIdentity } from "#/modules/runtime/data-loss-identity";
import {
  clusterRead,
  OrganizationRuntime,
  operationRuntimeFrame,
  RUNTIME_FRAME_TIMEOUT_MS,
  withClusterBudget,
} from "#/modules/runtime/organization-runtime.server";
import { dockerVolumeName } from "#/modules/volume-run/volume-run";
import { Conflict } from "#/server/public-error";

export const UNREACHABLE_MESSAGE = "Your servers aren't answering. Try again once they are.";
const unreachable = () => new Conflict({ userFacing: true, message: UNREACHABLE_MESSAGE });

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

/** An operation as the gate weighs it, with the services a Drain of it acts on and those it moves to another Server. */
export type DrainPlan = OperationAsked & { readonly targets: QualifiedService[]; readonly moves: QualifiedService[] };

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
    // Its attempt keeps the approval, so carrying it again answers that drain.
    recorded: true,
    preview: { server: machineId, moves, stays, retires },
    effects,
    targets: [...moves, ...stays, ...retires].sort(byName),
    moves,
  };
}

const servesWeb = ({ machine }: RuntimeWatchView["machines"][number]) => machine.accepts_ingress && machine.public_ip !== null;
const online = ({ machine, membership }: RuntimeWatchView["machines"][number]) =>
  !needsAttention(serverStatus({ membership, runningBuilds: machine.runtime.running_builds }));

/**
 * What turning web traffic off on `machineId` loses: every domain the Cluster serves, when no other online Server with
 * a public IP takes web traffic. The preview is the whole `change`, so approving web off approves nothing else it carries.
 * Null when the frame has no such Server.
 */
export function ingressOffPlan(frame: RuntimeWatchView, machineId: string, change: ServerPolicyChange): OperationAsked | null {
  const name = serverName(frame, machineId);
  if (name === undefined) return null;
  const last = frame.machines.some((observed) => observed.machine.id === machineId && servesWeb(observed))
    && !frame.machines.some((observed) => observed.machine.id !== machineId && servesWeb(observed) && online(observed));
  const domains = last ? [...new Set(frame.certificates.map(({ hostname }) => hostname))].sort(byName) : [];
  return {
    subject: `server:${machineId}`,
    verb: "set",
    name,
    preview: { server: machineId, ...machineUpdateForPolicyChange(change) },
    effects: domains.map((hostname) => ({ kind: "removes_domain", name: hostname, node: hostname, path: `domains/${hostname}` })),
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
    // Its clean keeps the approval, so carrying it again answers that clean.
    recorded: true,
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

type Copy = RuntimeWatchView["volumes"][number];
/** Ployz's own buildkit cache, which `ployz server rm` doesn't count as data loss either. */
const builderCache = /^buildx_buildkit_ployz-[0-9]+_state$/;
const oldCopy = ({ storage }: Copy) =>
  storage.kind === "provisioned" && storage.role === "slot" && (storage.slot_role ?? "mirror") === "old_copy";
/** A writer, or a mirror; a root mid-switch may be a pull that never finished. */
const keeps = ({ storage }: Copy) =>
  storage.kind === "provisioned" && (storage.role === "slot" ? (storage.slot_role ?? "mirror") === "mirror" : storage.role !== "switching");

/**
 * What forgetting `machineId` loses, from one frame: each Docker volume on it, since its identity is its Machine's, and
 * each Managed volume copy on it that no other Server holds as a writer or a mirror. An Old copy is older than the
 * Volume it was cut from, so it neither counts as lost nor keeps one.
 */
export function forgetLoses(frame: RuntimeWatchView, machineId: string): DataLossIdentity[] {
  const kept = new Set(frame.volumes
    .filter((copy) => copy.id.machine_id !== machineId && keeps(copy))
    .map(({ id }) => id.name));
  return frame.volumes
    .filter((copy) => copy.id.machine_id === machineId && !builderCache.test(copy.id.name)
      && (copy.storage.kind !== "provisioned" || (!oldCopy(copy) && !kept.has(copy.id.name))))
    .map(({ id }) => ({ kind: "docker_volume" as const, id }));
}

/** A reset removal loses access to the Volumes it confirmed; a forget loses those `forgetLoses` found. */
export type Removal = { readonly reset: boolean; readonly volumes: readonly DataLossIdentity[] };

export function removePlan(
  machineId: string,
  name: string,
  { reset, volumes: lost }: Removal,
  labels: ReadonlyMap<string, string> = new Map(),
): OperationAsked {
  const volumes = [...lost].sort((a, b) => byName(volumeName(a), volumeName(b)));
  return {
    subject: `server:${machineId}`,
    verb: reset ? "remove" : "forget",
    name,
    preview: { server: machineId, reset, volumes: volumes.map(volumeName) },
    effects: [
      { kind: "removes_server", name, node: machineId, path: `servers/${machineId}` },
      ...volumes.map((identity) => deletesVolume(identity, labels.get(identity.id.name))),
    ],
  };
}

const observe = (organizationId: string) => operationRuntimeFrame(organizationId).pipe(
  Effect.mapError(unreachable),
  Effect.flatMap((frame) => frame === null ? Effect.fail(unreachable()) : Effect.succeed(frame)),
);

export const planDrain = Effect.fn("ServerOperations.planDrain")(function* (organizationId: string, machineId: string) {
  const frame = yield* observe(organizationId);
  return drainPlan(frame, new Set(yield* ownedNamespaces(organizationId)), machineId);
});

export const planIngressOff = Effect.fn("ServerOperations.planIngressOff")(function* (
  organizationId: string, machineId: string, change: ServerPolicyChange,
) {
  return ingressOffPlan(yield* observe(organizationId), machineId, change);
});

const ownedOrSystem = (organizationId: string, namespace: string) => namespace === SYSTEM_NAMESPACE
  ? Effect.succeed(true)
  : ownedNamespaces(organizationId).pipe(Effect.map((owned) => owned.includes(namespace)));

/** How long a clean waits for the Cluster to say what destroying a Namespace would lose. */
const DATA_LOSS_TIMEOUT = "10 seconds";

/** A clean of `namespace` as it stands now; only of one no Environment owns, and never Ployz's own. */
export const planClean = Effect.fn("ServerOperations.planClean")(function* (organizationId: string, namespace: string) {
  if (yield* ownedOrSystem(organizationId, namespace)) {
    return yield* new Conflict({ userFacing: true, message: `${namespace} belongs to an Environment: delete that Environment instead.` });
  }
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  if (session.status !== "connected") return yield* unreachable();
  const lost = yield* session.connected.dataLossIfNamespaceDestroyed(namespace).pipe(
    clusterRead,
    Effect.mapError(unreachable),
    Effect.timeoutOrElse({ duration: DATA_LOSS_TIMEOUT, orElse: () => Effect.fail(unreachable()) }),
  );
  const frame = yield* session.connected.watchFirstFrame(RUNTIME_FRAME_TIMEOUT_MS).pipe(clusterRead, Effect.mapError(unreachable));
  return cleanPlan(frame, namespace, lost.data_loss);
}, Effect.scoped);

type Unreadable = { readonly namespace: string; readonly project: string; readonly environment: string };

/** Who owns each doomed Docker volume, and the Environments holding one that the Store can't read back. */
const volumeOwners = Effect.fn("ServerOperations.volumeOwners")(function* (
  organizationId: string,
  doomed: readonly DataLossIdentity[],
) {
  const names = new Set(doomed.map(({ id }) => id.name));
  const owners: VolumeOwner[] = [];
  const unreadables: Unreadable[] = [];
  if (names.size === 0) return { owners, unreadables };
  const { namespaces } = yield* readStore(organizationId, { query: "namespaces" });
  for (const owned of namespaces) {
    if (![...names].some((name) => name.startsWith(`${owned.namespace}_`))) continue;
    const read = yield* readStore(organizationId, {
      query: "volumes",
      environment: { project: owned.project, environment: owned.environment },
    }).pipe(Effect.map(({ volumes }) => volumes), Effect.catchIf(unreadable, () => Effect.succeed(null)));
    if (read === null) {
      unreadables.push(owned);
      continue;
    }
    for (const volume of read) {
      const docker = dockerVolumeName(owned.namespace, volume.id);
      if (names.has(docker)) owners.push({ dockerName: docker, project: owned.project, environment: owned.environment, volume: volume.name });
    }
  }
  return { owners, unreadables };
});

/** A lost Docker volume as the Remove server dialog lists it: an Environment's Volume by its name, and where it lives. */
export type LostVolumeName = { readonly docker: string; readonly name: string; readonly detail?: string };

/** Docker names a volume no one named by a 64-hex id; it backs a container's anonymous mount. */
const ANONYMOUS_VOLUME = /^[0-9a-f]{64}$/;

export const nameLostVolumes = Effect.fn("ServerOperations.nameLostVolumes")(function* (
  organizationId: string,
  lost: readonly DataLossIdentity[],
) {
  // Names only label what the Server answered; a Config Store that can't be read leaves the Docker names, not no list.
  const found = yield* volumeOwners(organizationId, lost).pipe(
    Effect.catch((error) => Effect.logWarning("Could not name lost volumes.", { error }).pipe(Effect.as({ owners: [], unreadables: [] }))),
  );
  const owners = new Map(found.owners.map((owner) => [owner.dockerName, owner]));
  return lost.map(({ id: { name: docker } }): LostVolumeName => {
    const owner = owners.get(docker);
    if (owner !== undefined) return { docker, name: owner.volume, detail: `${owner.project}/${owner.environment}` };
    const unread = found.unreadables.find(({ namespace }) => docker.startsWith(`${namespace}_`));
    if (unread !== undefined) return { docker, name: "Unreadable volume", detail: `${unread.project}/${unread.environment}` };
    return ANONYMOUS_VOLUME.test(docker) ? { docker, name: "Anonymous volume", detail: docker.slice(0, 12) } : { docker, name: docker };
  });
});

export const planRemove = Effect.fn("ServerOperations.planRemove")(function* (
  organizationId: string,
  machineId: string,
  confirmed: readonly DataLossIdentity[] | null,
) {
  const frame = yield* observe(organizationId);
  const name = serverName(frame, machineId);
  if (name === undefined) return null;
  const removal = confirmed === null ? { reset: false, volumes: forgetLoses(frame, machineId) } : { reset: true, volumes: confirmed };
  const { owners, unreadables: [unread] } = yield* volumeOwners(organizationId, removal.volumes);
  // Fail closed: the Server may hold the only copy of a Volume nobody can name.
  if (unread !== undefined) return yield* unreadableEnvironment(unread);
  const owned = volumeLabels(owners);
  const labels = new Map(removal.volumes.map((identity) =>
    [identity.id.name, owned.get(identity.id.name) ?? volumeMountLabel(frame, identity)]));
  return removePlan(machineId, name, removal, labels);
});

/**
 * An operation approval's preview recomputed on read, so a changed Cluster supersedes it. A Server no longer observed,
 * or a Namespace an Environment owns by now, no longer applies. When the Cluster can't be read within 5 seconds of Cluster
 * time, the stored preview stands.
 */
export const freshOperationDigest: OperationDigest<Effect.Services<ReturnType<typeof planClean>>> = (organizationId, subject, verb, stored, preview) => {
  const separator = subject.indexOf(":");
  const key = subject.slice(separator + 1);
  // A member, invite, Service, Project, Environment, Organization or forget-every-Server approval is planned again when
  // its command reruns; there is no Cluster to recompute it from.
  if (["member", "invite", "service", "project", "environment", "organization", "servers"].includes(subject.slice(0, separator))) return Effect.succeed(stored);
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
      case "set": {
        const plan = ingressOffPlan(yield* observe(organizationId), key, { acceptsIngress: false });
        return plan === null || plan.effects.length === 0 ? null : operationDigest({ ...plan, preview });
      }
      case "remove":
      case "forget": {
        const frame = yield* observe(organizationId);
        return frame.machines.some(({ machine }) => machine.id === key) ? stored : null;
      }
      case "release":
      case "remove_mirror":
      case "move":
      case "restore":
      case "resize":
      case "stop":
      case "restart":
      case "unprotect":
        return stored;
    }
  });
  return fresh.pipe(withClusterBudget(5_000), Effect.orElseSucceed(() => stored));
};

/** Every approval still waiting in the Organization, each operation's preview recomputed against the Cluster first. */
export const freshPendingApprovals = (organizationId: string) => pendingApprovals(organizationId, freshOperationDigest);

/** One approval in the Organization, its operation's preview recomputed against the Cluster first. */
export const freshApproval = (organizationId: string, id: string) => getApproval(organizationId, id, freshOperationDigest);
