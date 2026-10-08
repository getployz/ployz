import "@tanstack/react-start/server-only";
import type { DestructiveEffect, QualifiedService, RuntimeWatchView } from "@ployz/sdk";
import { Effect } from "effect";
import { operationDigest, type OperationDigest } from "#/modules/approvals/approvals.server";
import type { OperationAsked } from "#/modules/approvals/approvals.server";
import { ownedNamespaces } from "#/modules/machines/namespace-cleanup.server";
import { SYSTEM_NAMESPACE, splitQualifiedService } from "#/modules/machines/server-services";
import type { DataLossIdentity } from "#/modules/runtime/data-loss-identity";
import { firstRuntimeFrame, OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { Conflict, NotFound } from "#/server/public-error";

const unreachable = () => new Conflict({ userFacing: true, message: "Your servers aren't answering. Try again once they are." });

const byName = (a: string, b: string) => (a < b ? -1 : a > b ? 1 : 0);
const volumeName = ({ id }: DataLossIdentity) => `${id.machine_id}/${id.name}`;
const deletesVolume = (identity: DataLossIdentity): DestructiveEffect => ({
  kind: "deletes_volume",
  name: identity.id.name,
  node: volumeName(identity),
  path: `volumes/${volumeName(identity)}`,
});
const removesService = (identity: QualifiedService, serviceId: string): DestructiveEffect => ({
  kind: "removes_service",
  name: identity,
  node: serviceId,
  path: `services/${identity}`,
});
const serverName = (frame: RuntimeWatchView | null, machineId: string) =>
  frame?.machines.find(({ machine }) => machine.id === machineId)?.machine.name ?? machineId;

/** An operation as the gate weighs it, with the services a Drain of it acts on. */
export type DrainPlan = OperationAsked & { readonly targets: QualifiedService[] };

/**
 * What draining `machineId` does to the Services the Organization's Environments own there, from one frame: each moves,
 * stays (a Volume holds it; its data never moves), or retires (a Global slot). A Global with no running slot elsewhere
 * is removed outright. Null when the frame has no such Server.
 */
export function drainPlan(frame: RuntimeWatchView, owned: ReadonlySet<string>, machineId: string): DrainPlan | null {
  if (!frame.machines.some(({ machine }) => machine.id === machineId)) return null;
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
    name: serverName(frame, machineId),
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
): OperationAsked & { readonly confirmDataLoss: DataLossIdentity[] } {
  const services = frame.services.filter((service) =>
    splitQualifiedService(service.identity).namespace === namespace
    && service.containers.some((container) => container.kind === "service_container"));
  const running = services.filter((service) =>
    service.containers.some((container) => container.kind === "service_container" && container.runtime.state === "running"));
  const confirmDataLoss = [...volumes].sort((a, b) => byName(volumeName(a), volumeName(b)));
  return {
    subject: `namespace:${namespace}`,
    verb: "clean",
    name: namespace,
    preview: {
      namespace,
      services: services.map(({ identity }) => identity).sort(byName),
      volumes: confirmDataLoss.map(volumeName),
    },
    effects: [
      ...running.map((service) => removesService(service.identity, service.service_id)).sort((a, b) => byName(a.path, b.path)),
      ...confirmDataLoss.map(deletesVolume),
    ],
    confirmDataLoss,
  };
}

/** Removing a Server: the Server itself, and each Volume its removal confirms losing. */
export function removePlan(machineId: string, name: string, confirmed: readonly DataLossIdentity[]): OperationAsked {
  const volumes = [...confirmed].sort((a, b) => byName(volumeName(a), volumeName(b)));
  return {
    subject: `server:${machineId}`,
    verb: "remove",
    name,
    preview: { server: machineId, volumes: volumes.map(volumeName) },
    effects: [
      { kind: "removes_server", name, node: machineId, path: `servers/${machineId}` },
      ...volumes.map(deletesVolume),
    ],
  };
}

const observe = (organizationId: string) => firstRuntimeFrame(organizationId).pipe(
  Effect.mapError(unreachable),
  Effect.flatMap((frame) => frame === null ? Effect.fail(unreachable()) : Effect.succeed(frame)),
);

/** A Drain of `machineId` as it stands now. */
export const planDrain = Effect.fn("ServerOperations.planDrain")(function* (organizationId: string, machineId: string) {
  const frame = yield* observe(organizationId);
  const plan = drainPlan(frame, new Set(yield* ownedNamespaces(organizationId)), machineId);
  if (plan === null) return yield* new NotFound({ message: "No such Server." });
  return plan;
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
  const frame = yield* observe(organizationId);
  return cleanPlan(frame, namespace, lost.data_loss);
}, Effect.scoped);

/** A Server removal; the Server's name from one frame when the Cluster answers, else its ID. */
export const planRemove = Effect.fn("ServerOperations.planRemove")(function* (
  organizationId: string,
  machineId: string,
  confirmed: readonly DataLossIdentity[],
) {
  const frame = yield* firstRuntimeFrame(organizationId).pipe(Effect.orElseSucceed(() => null));
  return removePlan(machineId, serverName(frame, machineId), confirmed);
});

/**
 * An operation approval's preview recomputed on read, so a changed Cluster supersedes it. A Server no longer observed,
 * or a Namespace an Environment owns by now, no longer applies. When the Cluster can't be read, the stored preview stands.
 */
export const freshOperationDigest: OperationDigest<Effect.Services<ReturnType<typeof planClean>>> = (organizationId, subject, operation) => {
  const stored = operationDigest(operation.verb, operation.preview);
  const separator = subject.indexOf(":");
  const key = subject.slice(separator + 1);
  const fresh = Effect.gen(function* () {
    switch (operation.verb) {
      case "drain": {
        const frame = yield* observe(organizationId);
        const plan = drainPlan(frame, new Set(yield* ownedNamespaces(organizationId)), key);
        return plan === null ? null : operationDigest("drain", plan.preview);
      }
      case "clean":
        if (yield* ownedOrSystem(organizationId, key)) return null;
        return operationDigest("clean", (yield* planClean(organizationId, key)).preview);
      case "remove": {
        const frame = yield* observe(organizationId);
        return frame.machines.some(({ machine }) => machine.id === key) ? stored : null;
      }
    }
  });
  return fresh.pipe(Effect.orElseSucceed(() => stored));
};
