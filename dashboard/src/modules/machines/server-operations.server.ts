import "@tanstack/react-start/server-only";
import type { DestructiveEffect, QualifiedService, RuntimeWatchView } from "@ployz/sdk";
import { Effect } from "effect";
import { SERVERS_UNANSWERED_MESSAGE } from "#/lib/refusal";
import type { OperationAsked } from "#/modules/operations/asked";
import { readStore } from "#/modules/config-store/config-store.server";
import { unreadable, unreadableEnvironment } from "#/modules/config-store/store-lookup.server";
import { ownedNamespaces } from "#/modules/config-store/store-namespaces.server";
import { machineUpdateForPolicyChange, type ServerPolicyChange } from "#/modules/machines/server-policy";
import { dataCheck, type DataPlan, dataPlan } from "#/modules/machines/server-drain-volumes";
import { globalsStopDrain } from "#/modules/machines/server-drain-view";
import { SYSTEM_NAMESPACE, splitQualifiedService } from "#/modules/machines/server-services";
import { needsAttention, serverStatus } from "#/modules/machines/server-status";
import type { DataLossIdentity } from "#/modules/runtime/data-loss-identity";
import {
  clusterRead,
  OrganizationRuntime,
  operationRuntimeFrame,
  RUNTIME_FRAME_TIMEOUT_MS,
} from "#/modules/runtime/organization-runtime.server";
import { dockerVolumeName } from "#/modules/runtime/naming";
import type { Settle } from "#/modules/runtime/ployz.server";
import { Conflict } from "#/server/public-error";

const unreachable = () => new Conflict({ userFacing: true, message: SERVERS_UNANSWERED_MESSAGE });

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

/**
 * Whether the frame's entry heard a Server's daemon answer: `answered`, or `unanswered` once it missed the entry's last
 * probes, when its rows are only as it last reported them. `pending` until the entry has heard either way, as in a
 * watch's first frame, sent before any probe. `unsaid` from an entry too old to name who answered, which names only who
 * missed.
 */
export type Heard = "answered" | "unanswered" | "pending" | "unsaid";

export const heardFrom = (frame: RuntimeWatchView, machineId: string): Heard =>
  frame.unanswered.some((id) => id === machineId) ? "unanswered"
  : frame.answered === undefined ? "unsaid"
  : frame.answered.some((id) => id === machineId) ? "answered"
  : "pending";

/**
 * How long a Drain reads frames for the entry to hear the drained Server. A watch's first frame comes before any probe,
 * and the entry names a Server unanswered after two missed probes five seconds apart.
 */
const DRAIN_HEARD_MS = 20_000;

/** The frame a Drain plans from: the first that says whether the drained Server answers, or that no longer lists it. */
export const drainSettle = (machineId: string): Settle => ({
  settled: (frame) => serverName(frame, machineId) === undefined || heardFrom(frame, machineId) !== "pending",
  patienceMs: DRAIN_HEARD_MS,
});

/**
 * An operation as the gate weighs it, with the services a Drain of it acts on and those it moves to another Server.
 * `globals` and `heard` refuse it (see `drainRefusal`). `data` is what the frame says of each Service held by its data
 * here: whether it may move with its Volume. It is a forecast the drain checks again, so it stays out of the preview
 * the approval signs.
 */
export type DrainPlan = OperationAsked & {
  readonly targets: QualifiedService[];
  readonly moves: QualifiedService[];
  readonly globals: QualifiedService[];
  readonly heard: Heard;
  readonly data: Readonly<Record<string, DataPlan>>;
};

/**
 * What draining `machineId` does to the Services the Organization's Environments own there, from one frame: each moves,
 * or stays (a Volume holds it, which the drain may then move with it). Any user Service there with a Global copy
 * anywhere refuses the Drain, owned or not, as the Engine's does. Null when the frame has no such Server.
 */
export function drainPlan(frame: RuntimeWatchView, owned: ReadonlySet<string>, machineId: string): DrainPlan | null {
  const name = serverName(frame, machineId);
  if (name === undefined) return null;
  const moves: QualifiedService[] = [];
  const stays: QualifiedService[] = [];
  const globals: QualifiedService[] = [];
  for (const service of frame.services) {
    const { namespace } = splitQualifiedService(service.identity);
    if (namespace === null || namespace === SYSTEM_NAMESPACE) continue;
    const slots = service.containers.filter((container) => container.kind === "service_container");
    const here = slots.filter((container) => container.machine_id === machineId);
    const newest = here.reduce<(typeof here)[number] | undefined>(
      (latest, container) => latest === undefined || container.created_at_unix_nanos > latest.created_at_unix_nanos ? container : latest,
      undefined,
    );
    if (newest === undefined) continue;
    if (service.containers.some((container) => container.resolved_spec.mode.mode === "global")) {
      globals.push(service.identity);
    } else if (!owned.has(namespace)) {
      continue;
    } else if (newest.resolved_spec.volumes.length > 0) {
      stays.push(service.identity);
    } else {
      moves.push(service.identity);
    }
  }
  [moves, stays, globals].forEach((list) => list.sort(byName));
  const data = Object.fromEntries(stays.map((service) => [service, dataPlan(dataCheck(frame, service, machineId))]));
  return {
    subject: `server:${machineId}`,
    verb: "drain",
    name,
    // Its attempt keeps the approval, so carrying it again answers that drain.
    preview: { server: machineId, moves, stays },
    effects: [],
    targets: [...moves, ...stays].sort(byName),
    moves,
    globals,
    heard: heardFrom(frame, machineId),
    data,
  };
}

/** Why a Drain of a Server the frame doesn't list, or that didn't answer it, is refused: nothing says what runs there. */
export const unseenDrain = (name: string | null) =>
  `${name ?? "The server"} isn't answering, so Cloud can't tell what draining it would stop. Try again once it answers.`;

/**
 * Why a Drain of the Server can't go ahead from the frame `plan` read, or null when it may. A Server the entry didn't
 * hear answer is refused, whether it missed the entry's probes or the entry hadn't heard it yet, as the Engine refuses
 * one it can't list: its rows may miss a Global that runs there now. An entry too old to say leaves that to the
 * Engine's listing. So is one that runs a Service with a Global copy: a copy stays routed while it crash-loops, so the
 * one here could be the last that works.
 */
export const drainRefusal = (plan: DrainPlan) => {
  if (plan.heard === "unanswered" || plan.heard === "pending") {
    return new Conflict({ userFacing: true, message: unseenDrain(plan.name) });
  }
  if (plan.globals.length === 0) return null;
  return new Conflict({ userFacing: true, message: globalsStopDrain(plan.globals.map((service) => splitQualifiedService(service).name)) });
};

const servesWeb = ({ machine }: RuntimeWatchView["machines"][number]) => machine.accepts_ingress && machine.public_ip !== null;
const online = ({ machine, membership }: RuntimeWatchView["machines"][number], unanswered: readonly string[]) =>
  !needsAttention(serverStatus({ membership, runningBuilds: machine.runtime.running_builds, unanswered: unanswered.includes(machine.id) }));

/**
 * What turning web traffic off on `machineId` loses: every domain the Cluster serves, when no other online Server with
 * a public IP takes web traffic. The preview is the whole `change`, so approving web off approves nothing else it carries.
 * Null when the frame has no such Server.
 */
export function ingressOffPlan(frame: RuntimeWatchView, machineId: string, change: ServerPolicyChange): OperationAsked | null {
  const name = serverName(frame, machineId);
  if (name === undefined) return null;
  const last = frame.machines.some((observed) => observed.machine.id === machineId && servesWeb(observed))
    && !frame.machines.some((observed) => observed.machine.id !== machineId && servesWeb(observed) && online(observed, frame.unanswered));
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

/** A removal, `ployz server rm` or `forget`, and the Volumes only that Server holds, which the Cluster loses with it. */
export type Removal = { readonly verb: "remove" | "forget"; readonly volumes: readonly DataLossIdentity[] };

export function removePlan(
  machineId: string,
  name: string,
  { verb, volumes: lost }: Removal,
  labels: ReadonlyMap<string, string> = new Map(),
): OperationAsked {
  const volumes = [...lost].sort((a, b) => byName(volumeName(a), volumeName(b)));
  return {
    subject: `server:${machineId}`,
    verb,
    name,
    preview: { server: machineId, volumes: volumes.map(volumeName) },
    effects: [
      { kind: "removes_server", name, node: machineId, path: `servers/${machineId}` },
      ...volumes.map((identity) => deletesVolume(identity, labels.get(identity.id.name))),
    ],
  };
}

export const observe = (organizationId: string, settle?: Settle) => operationRuntimeFrame(organizationId, undefined, settle).pipe(
  Effect.mapError(unreachable),
  Effect.flatMap((frame) => frame === null ? Effect.fail(unreachable()) : Effect.succeed(frame)),
);

export const planDrain = Effect.fn("ServerOperations.planDrain")(function* (organizationId: string, machineId: string) {
  const frame = yield* observe(organizationId, drainSettle(machineId));
  return drainPlan(frame, new Set(yield* ownedNamespaces(organizationId)), machineId);
});

export const planIngressOff = Effect.fn("ServerOperations.planIngressOff")(function* (
  organizationId: string, machineId: string, change: ServerPolicyChange,
) {
  return ingressOffPlan(yield* observe(organizationId), machineId, change);
});

export const ownedOrSystem = (organizationId: string, namespace: string) => namespace === SYSTEM_NAMESPACE
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
  const removal = { verb: confirmed === null ? "forget" : "remove", volumes: forgetLoses(frame, machineId) } as const;
  if (confirmed !== null) {
    const known = new Set(confirmed.map(volumeName));
    const unconfirmed = removal.volumes.filter((identity) => !known.has(volumeName(identity)));
    if (unconfirmed.length > 0) {
      return yield* new Conflict({
        userFacing: true,
        message: `Server ${name} holds the only copy of ${unconfirmed.map(({ id }) => id.name).join(", ")}, which wasn't confirmed. No changes made. Confirm the removal again.`,
      });
    }
  }
  const { owners, unreadables: [unread] } = yield* volumeOwners(organizationId, removal.volumes);
  // Fail closed: the Server may hold the only copy of a Volume nobody can name.
  if (unread !== undefined) return yield* unreadableEnvironment(unread);
  const owned = volumeLabels(owners);
  const labels = new Map(removal.volumes.map((identity) =>
    [identity.id.name, owned.get(identity.id.name) ?? volumeMountLabel(frame, identity)]));
  return removePlan(machineId, name, removal, labels);
});
