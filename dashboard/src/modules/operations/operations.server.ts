import "@tanstack/react-start/server-only";
import { createHash } from "node:crypto";
import type { AcceptedLoss, DestructiveEffect, EnvironmentRef, MachineId, ServerRef, ServiceVerb } from "@ployz/sdk";
import { Effect, Schema } from "effect";
import { Uuid } from "#/lib/schema";
import { askAgain, gateOperation, operationDigest, type OperationAsked } from "#/modules/approvals/approvals.server";
import { callerEnvironments, type Caller } from "#/modules/identity/actor";
import { machineRemoveAttemptConsuming } from "#/modules/machines/machine-removal.repository";
import { startMachineRemove } from "#/modules/machines/machine-removal.server";
import { requestNamespaceCleanup } from "#/modules/machines/namespace-cleanup.server";
import { requestCliServerDrain } from "#/modules/machines/server-drain.server";
import type { ServerPolicyChange } from "#/modules/machines/server-policy";
import { planKeyedServerPolicyChange } from "#/modules/machines/server-policy.server";
import { planClean, planDrain, planIngressOff, planRemove } from "#/modules/machines/server-operations.server";
import { admitOperation, type Admission } from "#/modules/operations/keyed.server";
import { OPERATIONS, VOLUME_OPERATIONS } from "#/modules/operations/operations";
import type { DataLossIdentity } from "#/modules/runtime/data-loss-identity";
import { beyondEnvironments, lacksPermission } from "#/modules/organization/access.server";
import type { Permission } from "#/modules/organization/permissions";
import { requestServiceAct, serviceActExists } from "#/modules/service-act/service-act.server";
import { planCliServerUpgrade } from "#/modules/server-upgrade/server-upgrade.server";
import type { VolumeRunInput } from "#/modules/volume-run/volume-run";
import { admitRun, planVolumeRun } from "#/modules/volume-run/volume-run.server";
import { Conflict, Forbidden, NotFound } from "#/server/public-error";
import { uuidFrom } from "#/server/uuid-from";

export type OperationCall = {
  readonly key: string | null;
  readonly approvalId: string | null;
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
  return loss === null ? gateOperation(caller, call.approvalId, asked) : Effect.succeed(loss);
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

/** `run`, unless `caller` may not run an operation that `requires` a permission. */
const holding = <A, E, R>(caller: Caller, requires: Permission, environment: string | null | undefined, run: Effect.Effect<A, E, R>) =>
  Effect.suspend((): Effect.Effect<A | ReturnType<typeof refused>, E, R> => {
    const refusal = unheld(caller, requires, environment);
    return refusal === null ? run : Effect.succeed(refusal);
  });

/** Null when no Server of the Organization has `machineId`. */
export const drainServer = (caller: Caller, machineId: MachineId, call: OperationCall) =>
  holding(caller, OPERATIONS["server.drain"].requires, undefined, Effect.gen(function* () {
    const plan = yield* planDrain(caller.organization.id, machineId);
    if (plan === null) return null;
    return yield* admitOperation(caller.organization.id, { tool: OPERATIONS["server.drain"].idempotency, key: call.key, body: { machineId } }, Effect.gen(function* () {
      const gated = yield* gate(caller, call, plan);
      if (!gated.ok) return refused(gated.refusal);
      return (attemptId: string) => requestCliServerDrain(actor(caller), {
        attemptId, machineId, targets: plan.targets, approvalId: gated.approvalId, planDigest: operationDigest(plan),
      })
        .pipe(Effect.map((id) => ({ ok: true, id }) as const));
    }));
  }));

export const upgradeServer = (caller: Caller, input: { readonly machineId: MachineId; readonly channel: string | undefined }, call: OperationCall) =>
  holding(caller, OPERATIONS["server.upgrade"].requires, undefined,
    admitOperation(caller.organization.id, { tool: OPERATIONS["server.upgrade"].idempotency, key: call.key, body: input }, planCliServerUpgrade(actor(caller), input)));

export const setServer = (caller: Caller, machineId: MachineId, change: ServerPolicyChange, call: OperationCall) =>
  holding(caller, OPERATIONS["server.set"].requires, undefined, admitOperation(caller.organization.id, { tool: OPERATIONS["server.set"].idempotency, key: call.key, body: { machineId, change } }, Effect.gen(function* () {
    const plan = change.acceptsIngress === false ? yield* planIngressOff(caller.organization.id, machineId, change) : null;
    if (plan !== null) {
      const gated = yield* gate(caller, call, plan);
      if (!gated.ok) return refused(gated.refusal);
    }
    return yield* planKeyedServerPolicyChange(caller, machineId, change);
  })));

export const cleanNamespace = (caller: Caller, namespace: string, call: OperationCall) =>
  holding(caller, OPERATIONS["namespace.clean"].requires, undefined, admitOperation(caller.organization.id, { tool: OPERATIONS["namespace.clean"].idempotency, key: call.key, body: { namespace } }, Effect.gen(function* () {
    const plan = yield* planClean(caller.organization.id, namespace);
    const gated = yield* gate(caller, call, plan);
    if (!gated.ok) return refused(gated.refusal);
    return (attemptId: string) => requestNamespaceCleanup(actor(caller), {
      attemptId,
      namespace,
      confirmDataLoss: plan.doomed.map(({ identity }) => identity),
      approvalId: gated.approvalId,
    }).pipe(Effect.map((id) => ({ ok: true, id }) as const));
  })));

/** Resets Server `machineId`, deleting exactly the Data Loss the caller `confirmed`. Null when no Server has `machineId`. */
export const removeServer = (caller: Caller, machineId: MachineId, confirmed: readonly DataLossIdentity[], call: OperationCall) =>
  removal(caller, machineId, confirmed, call);

/** Takes Server `machineId` out of the Cluster without a reset, for a Server that is gone. Null when no Server has `machineId`. */
export const forgetServer = (caller: Caller, machineId: MachineId, call: OperationCall) =>
  removal(caller, machineId, null, call);

/** The dashboard's Forget of Server `machineId`: only a Caller who may remove Servers; answers the removal's id. */
export const forgetServerFromDashboard = (caller: Caller, machineId: MachineId, key: string) =>
  Effect.gen(function* () {
    const refusal = unheld(caller, OPERATIONS["server.forget"].requires);
    if (refusal !== null) return yield* new Forbidden({ message: refusal.refusal.message, userFacing: true });
    const admission = yield* forgetServer(caller, machineId, { key, approvalId: null });
    if (admission === null) return yield* new NotFound({ message: "The server was not found." });
    if (admission.ok) return { id: admission.id };
    return yield* new Conflict({ userFacing: true, message: admission.refusal.message });
  });

const removal = (caller: Caller, machineId: MachineId, confirmed: readonly DataLossIdentity[] | null, call: OperationCall) =>
  holding(caller, OPERATIONS[confirmed === null ? "server.forget" : "server.remove"].requires, undefined, Effect.gen(function* () {
    const noReset = confirmed === null;
    const consumed = Schema.is(Uuid)(call.approvalId)
      ? yield* machineRemoveAttemptConsuming(caller.organization.id, call.approvalId, machineId, noReset)
      : null;
    if (consumed !== null) return { ok: true, id: consumed } as const;
    const plan = yield* planRemove(caller.organization.id, machineId, confirmed);
    if (plan === null) return null;
    const tool = noReset ? OPERATIONS["server.forget"].idempotency : OPERATIONS["server.remove"].idempotency;
    const body = noReset ? { machineId } : { machineId, confirmed };
    return yield* admitOperation(caller.organization.id, { tool, key: call.key, body }, Effect.gen(function* () {
      const gated = yield* gate(caller, call, plan);
      if (!gated.ok) return refused(gated.refusal);
      return (id: string) => startMachineRemove({
        id,
        organizationId: caller.organization.id,
        requestedByUserId: caller.userId,
        machineId,
        approvalId: gated.approvalId,
        ...(noReset ? { confirmDataLoss: [], noReset: true } : { confirmDataLoss: [...confirmed] }),
      }).pipe(Effect.map((started) => ({ ok: true, id: started.id }) as const));
    }));
  }));

/** A Volume Run whose id is its attempt, so a replayed request reads back the run it started. */
export const runVolume = (
  caller: Caller,
  input: { readonly volumeId: string; readonly environment: EnvironmentRef } & VolumeRunInput,
  call: OperationCall,
) =>
  holding(caller, VOLUME_OPERATIONS[input.kind].requires, input.environment.environment, admitOperation(caller.organization.id, { tool: VOLUME_OPERATIONS[input.kind].idempotency, key: call.key, body: input }, Effect.gen(function* () {
    const planned = yield* planVolumeRun(caller.organization.id, input);
    if (!planned.ok) return refused(planned.refusal);
    if (planned.asked !== null) {
      const gated = yield* gate(caller, call, planned.asked);
      if (!gated.ok) return refused(gated.refusal);
    }
    return (runId: string) => admitRun(actor(caller), planned.input, planned.volume, runId)
      .pipe(Effect.map((inserted) => inserted.ok
        ? { ok: true, id: inserted.run.id } as const
        : { ok: false, refusal: inserted.refusal, busy: inserted.busy } as const));
  })));

/** What a stop or restart interrupts, as a human approves it; a start interrupts nothing. */
const interrupts = (input: {
  readonly environmentId: string;
  readonly place: string;
  readonly service: string;
  readonly verb: ServiceVerb;
  readonly only: ServerRef | null;
}): OperationAsked | null => {
  if (input.verb === "start") return null;
  return {
    subject: `service:${input.environmentId}/${input.service}`,
    verb: input.verb,
    name: `${input.service} in ${input.place}${input.only === null ? "" : ` on ${input.only.name}`}`,
    preview: { environment: input.environmentId, service: input.service, only: input.only === null ? null : { machine: input.only.machine } },
    effects: [{
      kind: input.verb === "stop" ? "stops_service" : "restarts_service",
      name: input.service,
      node: `${input.environmentId}/${input.service}`,
      path: `services.${input.service}`,
    }],
  };
};

export const actService = (
  caller: Caller,
  input: {
    readonly environmentId: string;
    readonly environment: string;
    readonly place: string;
    readonly service: string;
    readonly namespace: string;
    readonly runtime: string;
    readonly verb: ServiceVerb;
    readonly only: ServerRef | null;
    readonly digest: string;
  },
  call: OperationCall,
) =>
  holding(caller, OPERATIONS[`service.${input.verb}`].requires, input.environment, admitOperation(
    caller.organization.id,
    { tool: OPERATIONS[`service.${input.verb}`].idempotency, key: call.key, body: { environmentId: input.environmentId, service: input.service, only: input.only } },
    Effect.gen(function* () {
      const { environment: _environment, ...act } = input;
      const run = (attemptId: string) => requestServiceAct(actor(caller), { attemptId, ...act });
      const asked = interrupts(input);
      if (asked === null) return run;
      const gated = yield* gate(caller, call, asked);
      if (!gated.ok) return refused(gated.refusal);
      if (gated.approvalId === null) return run;
      // One approval runs one stop or restart: its run takes an id the approval names, so a later call asks again.
      const approved = uuidFrom(`ployz.approval:${caller.organization.id}:${gated.approvalId}`);
      if (yield* serviceActExists(caller.organization.id, approved)) return refused(yield* askAgain(caller, asked));
      return () => run(approved);
    }),
  ));
