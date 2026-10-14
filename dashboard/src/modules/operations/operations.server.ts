import "@tanstack/react-start/server-only";
import { createHash } from "node:crypto";
import type { AcceptedLoss, DestructiveEffect, EnvironmentRef, MachineId, ServerRef, ServiceVerb } from "@ployz/sdk";
import { Effect, Schema } from "effect";
import { Uuid } from "#/lib/schema";
import { gateOperation, operationDigest, type OperationAsked } from "#/modules/approvals/approvals.server";
import type { Caller } from "#/modules/identity/actor";
import { machineRemoveAttemptConsuming } from "#/modules/machines/machine-removal.repository";
import { startMachineRemove } from "#/modules/machines/machine-removal.server";
import { requestNamespaceCleanup } from "#/modules/machines/namespace-cleanup.server";
import { requestCliServerDrain } from "#/modules/machines/server-drain.server";
import { planClean, planDrain, planRemove } from "#/modules/machines/server-operations.server";
import { admitOperation, type Admission } from "#/modules/operations/keyed.server";
import { OPERATIONS, VOLUME_TOOLS } from "#/modules/operations/operations";
import type { DataLossIdentity } from "#/modules/runtime/data-loss-identity";
import { requestServiceAct } from "#/modules/service-act/service-act.server";
import { planCliServerUpgrade } from "#/modules/server-upgrade/server-upgrade.server";
import type { VolumeRunInput } from "#/modules/volume-run/volume-run";
import { insertRun, planVolumeRun } from "#/modules/volume-run/volume-run.server";

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
const actor = (caller: Caller) => ({ userId: caller.userId, organizationId: caller.organization.id });

/** Null when no Server of the Organization has `machineId`. */
export const drainServer = (caller: Caller, machineId: MachineId, call: OperationCall) =>
  Effect.gen(function* () {
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
  });

export const upgradeServer = (caller: Caller, input: { readonly machineId: string; readonly channel: string | undefined }, call: OperationCall) =>
  admitOperation(caller.organization.id, { tool: OPERATIONS["server.upgrade"].idempotency, key: call.key, body: input }, planCliServerUpgrade(actor(caller), input));

export const cleanNamespace = (caller: Caller, namespace: string, call: OperationCall) =>
  admitOperation(caller.organization.id, { tool: OPERATIONS["namespace.clean"].idempotency, key: call.key, body: { namespace } }, Effect.gen(function* () {
    const plan = yield* planClean(caller.organization.id, namespace);
    const gated = yield* gate(caller, call, plan);
    if (!gated.ok) return refused(gated.refusal);
    return (attemptId: string) => requestNamespaceCleanup(actor(caller), {
      attemptId,
      namespace,
      confirmDataLoss: plan.doomed.map(({ identity }) => identity),
      approvalId: gated.approvalId,
    }).pipe(Effect.map((id) => ({ ok: true, id }) as const));
  }));

/** Resets Server `machineId`, deleting exactly the Data Loss the caller `confirmed`. Null when no Server has `machineId`. */
export const removeServer = (caller: Caller, machineId: string, confirmed: readonly DataLossIdentity[], call: OperationCall) =>
  removal(caller, machineId, confirmed, call);

/** Takes Server `machineId` out of the Cluster without a reset, for a Server that is gone. Null when no Server has `machineId`. */
export const forgetServer = (caller: Caller, machineId: string, call: OperationCall) =>
  removal(caller, machineId, null, call);

const removal = (caller: Caller, machineId: string, confirmed: readonly DataLossIdentity[] | null, call: OperationCall) =>
  Effect.gen(function* () {
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
  });

/** A Volume Run whose id is its attempt, so a replayed request reads back the run it started. */
export const runVolume = (
  caller: Caller,
  input: { readonly volumeId: string; readonly environment: EnvironmentRef } & VolumeRunInput,
  call: OperationCall,
) =>
  admitOperation(caller.organization.id, { tool: VOLUME_TOOLS[input.kind], key: call.key, body: input }, Effect.gen(function* () {
    const planned = yield* planVolumeRun(caller.organization.id, input);
    if (!planned.ok) return refused(planned.refusal);
    if (planned.asked !== null) {
      const gated = yield* gate(caller, call, planned.asked);
      if (!gated.ok) return refused(gated.refusal);
    }
    return (runId: string) => insertRun(actor(caller), input, planned.volume, runId)
      .pipe(Effect.map((inserted) => inserted.ok ? { ok: true, id: inserted.run.id } as const : inserted));
  }));

export const actService = (
  caller: Caller,
  input: {
    readonly environmentId: string;
    readonly service: string;
    readonly namespace: string;
    readonly runtime: string;
    readonly verb: ServiceVerb;
    readonly only: ServerRef | null;
    readonly digest: string;
  },
  call: OperationCall,
) =>
  admitOperation(
    caller.organization.id,
    { tool: `service_${input.verb}`, key: call.key, body: { environmentId: input.environmentId, service: input.service, only: input.only } },
    Effect.succeed((attemptId: string) => requestServiceAct(actor(caller), { attemptId, ...input })),
  );
