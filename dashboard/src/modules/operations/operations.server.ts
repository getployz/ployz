import "@tanstack/react-start/server-only";
import { createHash } from "node:crypto";
import type { AcceptedLoss, DestructiveEffect, EnvironmentRef, MachineId, MachineName, ServerRef, ServiceVerb } from "@ployz/sdk";
import { Effect } from "effect";
import type { OperationAsked } from "#/modules/operations/asked";
import { agentRefusal, gateOperation, hasApprover, olderApproval, operationDigest } from "#/modules/operations/gate.server";
import { proposeServiceAct } from "#/modules/proposals/proposal-link.server";
import { callerEnvironments, type Caller } from "#/modules/identity/actor";
import { startMachineRemove } from "#/modules/machines/machine-removal.server";
import { requestNamespaceCleanup } from "#/modules/machines/namespace-cleanup.server";
import { requestCliServerDrain } from "#/modules/machines/server-drain.server";
import type { ServerPolicyChange } from "#/modules/machines/server-policy";
import { planKeyedServerPolicyChange } from "#/modules/machines/server-policy.server";
import { drainRefusal, planClean, planDrain, planIngressOff, planRemove } from "#/modules/machines/server-operations.server";
import { admitOperation, type Admission, replayed } from "#/modules/operations/keyed.server";
import { OPERATIONS, VOLUME_OPERATIONS } from "#/modules/operations/operations";
import type { DataLossIdentity } from "#/modules/runtime/data-loss-identity";
import { beyondEnvironments, lacksPermission } from "#/modules/organization/access.server";
import type { Permission } from "#/modules/organization/permissions";
import { activeServiceAct, busyRefusal, requestServiceAct } from "#/modules/service-act/service-act.server";
import { environmentProtected } from "#/modules/config-store/store-lookup.server";
import { protectorOnly } from "#/modules/config-store/protected.server";
import { planCliServerUpgrade } from "#/modules/server-upgrade/server-upgrade.server";
import type { VolumeRunInput } from "#/modules/volume-run/volume-run";
import { admitRun, type VolumeRunPlan } from "#/modules/volume-run/volume-run.rows.server";
import { planVolumeRun } from "#/modules/volume-run/volume-run.cluster.server";
import { Conflict, Forbidden, NotFound } from "#/server/public-error";

export type OperationCall = {
  readonly key: string | null;
  /** The digest a person said yes to, when Cloud asked for one. */
  readonly approved: string | null;
  readonly loss?: AcceptedLoss | null | undefined;
};

const refused = (refusal: Extract<Admission, { ok: false }>["refusal"]) => ({ ok: false, refusal }) as const;

const lossVersion = (effects: readonly DestructiveEffect[]) =>
  createHash("sha256").update(effects.map(({ node }) => node).sort().join("\n")).digest("hex").slice(0, 16);

function uncovered(asked: OperationAsked, call: OperationCall) {
  if (call.loss === undefined) return null;
  const effects = asked.effects.filter(({ kind }) => kind === "deletes_volume");
  if (effects.length === 0) return null;
  const accept = [...new Set(effects.map(({ name, node }) => name ?? node))].sort();
  const version = lossVersion(effects);
  const named = [...new Set(call.loss?.volumes ?? [])].sort();
  if (call.loss?.version === version && named.join("\n") === accept.join("\n")) return null;
  return refused({
    code: "confirmation_required",
    message: `This deletes the data in ${accept.join(", ")}. Nothing changed.`,
    details: { effects, accept, version, writes: "none" },
  });
}

const gate = (caller: Caller, call: OperationCall, asked: OperationAsked) => {
  const loss = uncovered(asked, call);
  return loss === null ? gateOperation(caller, call.approved, asked) : Effect.succeed(loss);
};
const actor = (caller: Caller) => ({ userId: caller.userId, organizationId: caller.organization.id, credential: caller.credential });

/**
 * Why `caller` may not run an operation that `requires` a permission, or null when they may. A token holds only what
 * its grant leaves of its maker's role, and writes only the Environments it names: never a Server-wide operation,
 * which has no `environment`.
 */
export const unheld = (caller: Caller, requires: Permission, environment?: string | null) => {
  const lacking = unpermitted(caller, requires);
  if (lacking !== null) return lacking;
  const environments = callerEnvironments(caller);
  const beyond = beyondEnvironments(environments, environment === undefined || environment === null ? "organization" : { environment });
  return beyond === null ? null : refused({ code: "forbidden", message: beyond, details: { environments: environments ?? [] } });
};

/** `unheld` for the permission alone: what an operation checks before it looks up where it lands. */
export const unpermitted = (caller: Caller, requires: Permission) => {
  const lacking = lacksPermission(caller, requires);
  return lacking === null ? null : refused({ code: "forbidden", message: lacking, details: { permission: requires } });
};

/**
 * `run`, unless `caller` may not run an operation that `requires` a permission, or carries an approval ID from an older
 * `ployz`: checked before a request key replays what it started, which never reaches the gate.
 */
const holding = <A, E, R>(caller: Caller, call: OperationCall, requires: Permission, environment: string | null | undefined, run: Effect.Effect<A, E, R>) =>
  Effect.gen(function* () {
    const refusal = unheld(caller, requires, environment);
    if (refusal !== null) return refusal;
    const older = olderApproval(call.approved);
    return older === null ? yield* run : refused(older);
  });

/**
 * Null when no Server of the Organization has `machineId`. Its run keeps `serverName`, the name the caller showed, else
 * the Server's name in the frame it planned from.
 */
export const drainServer = (caller: Caller, machineId: MachineId, call: OperationCall, serverName?: MachineName) =>
  holding(caller, call, OPERATIONS["server.drain"].requires, undefined, Effect.gen(function* () {
    const request = { tool: OPERATIONS["server.drain"].idempotency, key: call.key, body: { machineId } };
    const answered = yield* replayed(caller.organization.id, request);
    if (answered !== undefined) return answered;
    const plan = yield* planDrain(caller.organization.id, machineId);
    if (plan === null) return null;
    const refusal = drainRefusal(plan);
    if (refusal !== null) return yield* refusal;
    return yield* admitOperation(caller.organization.id, request, Effect.gen(function* () {
      const gated = yield* gate(caller, call, { ...plan, ployz: `ployz server drain ${plan.name}` });
      if (!gated.ok) return refused(gated.refusal);
      return (attemptId: string) => requestCliServerDrain(actor(caller), {
        attemptId, machineId, targets: plan.targets, planDigest: operationDigest(plan), approved: call.approved !== null, serverName: serverName || plan.name,
      })
        .pipe(Effect.map((id) => ({ ok: true, id }) as const));
    }));
  }));

export const upgradeServer = (
  caller: Caller, input: { readonly machineId: MachineId; readonly channel: string | undefined }, call: OperationCall, serverName?: MachineName,
) =>
  holding(caller, call, OPERATIONS["server.upgrade"].requires, undefined,
    admitOperation(caller.organization.id, { tool: OPERATIONS["server.upgrade"].idempotency, key: call.key, body: input }, Effect.gen(function* () {
      return yield* planCliServerUpgrade(actor(caller), input, serverName);
    })));

export const setServer = (caller: Caller, machineId: MachineId, change: ServerPolicyChange, call: OperationCall) =>
  holding(caller, call, OPERATIONS["server.set"].requires, undefined, admitOperation(caller.organization.id, { tool: OPERATIONS["server.set"].idempotency, key: call.key, body: { machineId, change } }, Effect.gen(function* () {
    const plan = change.acceptsIngress === false ? yield* planIngressOff(caller.organization.id, machineId, change) : null;
    if (plan !== null) {
      const gated = yield* gate(caller, call, { ...plan, ployz: `ployz server set ${plan.name} --accepts-ingress=false` });
      if (!gated.ok) return refused(gated.refusal);
    }
    return yield* planKeyedServerPolicyChange(caller, machineId, change);
  })));

export const cleanNamespace = (caller: Caller, namespace: string, call: OperationCall) =>
  holding(caller, call, OPERATIONS["namespace.clean"].requires, undefined, admitOperation(caller.organization.id, { tool: OPERATIONS["namespace.clean"].idempotency, key: call.key, body: { namespace } }, Effect.gen(function* () {
    const plan = yield* planClean(caller.organization.id, namespace);
    const gated = yield* gate(caller, call, { ...plan, ployz: `ployz server clean ${namespace}` });
    if (!gated.ok) return refused(gated.refusal);
    return (attemptId: string) => requestNamespaceCleanup(actor(caller), {
      attemptId,
      namespace,
      confirmDataLoss: plan.doomed.map(({ identity }) => identity),
    }).pipe(Effect.map((id) => ({ ok: true, id }) as const));
  })));

/** Takes Server `machineId` out of the Cluster, losing whatever only it holds unconfirmed. Null when no Server has `machineId`. */
export const forgetServer = (caller: Caller, machineId: MachineId, call: OperationCall, serverName?: MachineName) =>
  removeServer(caller, machineId, null, call, serverName);

/** The dashboard's Forget of Server `machineId`: only a Caller who may remove Servers; answers the removal's id. */
export const forgetServerFromDashboard = (caller: Caller, machineId: MachineId, key: string) =>
  Effect.gen(function* () {
    const refusal = unheld(caller, OPERATIONS["server.forget"].requires);
    if (refusal !== null) return yield* new Forbidden({ message: refusal.refusal.message, userFacing: true });
    const admission = yield* forgetServer(caller, machineId, { key, approved: null });
    if (admission === null) return yield* new NotFound({ message: "The server was not found." });
    if (admission.ok) return { id: admission.id };
    return yield* new Conflict({ userFacing: true, message: admission.refusal.message });
  });

/**
 * Takes Server `machineId` out of the Cluster and revokes its keys. The Volumes only it holds are lost: the caller
 * `confirmed` each of them, or, with `null` (a Forget), an approval covers them. Null when no Server has `machineId`. Its
 * run keeps `serverName`, the name the caller showed, else the Server's name in the frame it planned from.
 */
export const removeServer = (
  caller: Caller, machineId: MachineId, confirmed: readonly DataLossIdentity[] | null, call: OperationCall, serverName?: MachineName,
) =>
  holding(caller, call, OPERATIONS[confirmed === null ? "server.forget" : "server.remove"].requires, undefined, Effect.gen(function* () {
    const forget = confirmed === null;
    const tool = forget ? OPERATIONS["server.forget"].idempotency : OPERATIONS["server.remove"].idempotency;
    const body = forget ? { machineId } : { machineId, confirmed };
    // A removal's success takes the Server away, so a retry of its lost reply replays before looking for it.
    const answered = yield* replayed(caller.organization.id, { tool, key: call.key, body });
    if (answered !== undefined) return answered;
    const plan = yield* planRemove(caller.organization.id, machineId, confirmed);
    if (plan === null) return null;
    return yield* admitOperation(caller.organization.id, { tool, key: call.key, body }, Effect.gen(function* () {
      const gated = yield* gate(caller, call, { ...plan, ployz: `ployz server ${forget ? "forget" : "rm"} ${plan.name}` });
      if (!gated.ok) return refused(gated.refusal);
      return (id: string) => startMachineRemove({
        id,
        organizationId: caller.organization.id,
        requestedByUserId: caller.userId,
        machineId,
        serverName: serverName || plan.name,
        confirmDataLoss: forget ? [] : [...confirmed],
      }).pipe(Effect.map((started) => ({ ok: true, id: started.id }) as const));
    }));
  }));

/**
 * The ask a Volume Run makes: in a Protected Environment, one that interrupts or takes off what runs there is an owner's
 * or admin's to make, as it is refused to anyone who can't protect it.
 */
const guardedVolume = Effect.fn("Operations.guardedVolume")(function* (
  caller: Caller,
  { asked, guard, volume, environment }: Pick<Extract<VolumeRunPlan, { ok: true }>, "asked" | "guard" | "volume" | "environment">,
) {
  if (guard === null) return asked;
  if (protectorOnly(caller, environment.name) === null) return asked;
  if (!(yield* environmentProtected(caller.organization.id, environment.project, volume.environmentId))) return asked;
  return { ...guard, protected: environment.name } satisfies OperationAsked;
});

/** A Volume Run whose id is its attempt, so a replayed request reads back the run it started. */
export const runVolume = (
  caller: Caller,
  input: { readonly volumeId: string; readonly environment: EnvironmentRef } & VolumeRunInput,
  call: OperationCall,
) =>
  holding(caller, call, VOLUME_OPERATIONS[input.kind].requires, input.environment.environment, admitOperation(caller.organization.id, { tool: VOLUME_OPERATIONS[input.kind].idempotency, key: call.key, body: input }, Effect.gen(function* () {
    const planned = yield* planVolumeRun(caller.organization.id, input);
    if (!planned.ok) return refused(planned.refusal);
    // A restore drops the writes since its copy's last sync and asks no one, so an agent's waits for a person.
    if (input.kind === "restore" && caller.via === "agent" && (yield* hasApprover(caller.organization.id))) {
      const name = planned.volume.volumeName;
      return refused(agentRefusal({ verb: "restore", name, ployz: `ployz volume restore ${name} --from ${input.args.from}` }));
    }
    const asked = yield* guardedVolume(caller, planned);
    if (asked !== null) {
      const gated = yield* gate(caller, call, asked);
      if (!gated.ok) return refused(gated.refusal);
    }
    // A delete deletes only the copies the Servers reported as this person asked.
    const copies = "copies" in planned ? planned.copies : undefined;
    const who = copies === undefined ? actor(caller) : { ...actor(caller), confirmed: { by: "person" as const, copies: [...copies] } };
    return (runId: string) => admitRun(who, planned.input, planned.volume, runId)
      .pipe(Effect.map((inserted) => inserted.ok
        ? { ok: true, id: inserted.run.id } as const
        : { ok: false, refusal: inserted.refusal, busy: inserted.busy } as const));
  })));

/** What a stop or restart interrupts, as a person says yes to it; a start interrupts nothing. */
const interrupts = (input: {
  readonly environmentId: string;
  readonly place: string;
  readonly service: string;
  readonly namespace: string;
  readonly runtime: string;
  readonly verb: ServiceVerb;
  readonly only: ServerRef | null;
}): OperationAsked | null => {
  if (input.verb === "start") return null;
  return {
    subject: `service:${input.environmentId}/${input.service}`,
    verb: input.verb,
    name: `${input.service} in ${input.place}${input.only === null ? "" : ` on ${input.only.name}`}`,
    preview: {
      environment: input.environmentId,
      service: input.service,
      namespace: input.namespace,
      runtime: input.runtime,
      only: input.only === null ? null : { machine: input.only.machine } },
    effects: [{
      kind: input.verb === "stop" ? "stops_service" : "restarts_service",
      name: input.service,
      node: `${input.environmentId}/${input.service}`,
      path: `services.${input.service}`,
    }],
  };
};

/** Whether this stop or restart waits for someone who can protect its Protected Environment, rather than running. */
export const asksProtector = Effect.fn("Operations.asksProtector")(function* (
  caller: Caller,
  input: { readonly environmentId: string; readonly project: string; readonly environment: string; readonly verb: ServiceVerb },
) {
  if (input.verb === "start" || protectorOnly(caller, input.environment) === null) return false;
  return yield* environmentProtected(caller.organization.id, input.project, input.environmentId);
});

/**
 * Whether a stop or restart becomes a proposal a person decides rather than running: an agent's, unless only agents
 * own the Organization, and a member's in a Protected Environment they can't protect. A step of an approved proposal runs.
 */
const proposes = Effect.fn("Operations.proposes")(function* (
  caller: Caller,
  input: { readonly environmentId: string; readonly project: string; readonly environment: string; readonly verb: ServiceVerb; readonly proposal?: unknown },
) {
  if (input.verb === "start" || input.proposal !== undefined) return false;
  if (yield* asksProtector(caller, input)) return true;
  return caller.via === "agent" && (yield* hasApprover(caller.organization.id));
});

export const actService = (
  caller: Caller,
  input: {
    readonly environmentId: string;
    readonly project: string;
    readonly environment: string;
    readonly place: string;
    readonly service: string;
    readonly namespace: string;
    readonly runtime: string;
    readonly verb: ServiceVerb;
    readonly only: ServerRef | null;
    readonly digest: string;
    readonly proposal?: { readonly id: string; readonly step: number };
  },
  call: OperationCall,
) =>
  holding(caller, call, OPERATIONS[`service.${input.verb}`].requires, input.environment, admitOperation(
    caller.organization.id,
    { tool: OPERATIONS[`service.${input.verb}`].idempotency, key: call.key, body: { environmentId: input.environmentId, service: input.service, only: input.only } },
    Effect.gen(function* () {
      const { environment: _environment, project: _project, ...act } = input;
      const run = (attemptId: string) => requestServiceAct(actor(caller), { attemptId, ...act });
      const interrupted = interrupts(input);
      if (interrupted === null) return run;
      const busy = yield* activeServiceAct(caller.organization.id, input);
      if (busy !== null) return refused(busyRefusal(input.service, busy));
      if (yield* proposes(caller, input)) {
        const step = { verb: input.verb, service: input.service, only: input.only, namespace: input.namespace, runtime: input.runtime };
        return refused(yield* proposeServiceAct(caller, input, step));
      }
      const gated = yield* gate(caller, call, interrupted);
      if (!gated.ok) return refused(gated.refusal);
      return run;
    }),
  ));
