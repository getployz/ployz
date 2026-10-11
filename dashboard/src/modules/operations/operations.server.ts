import "@tanstack/react-start/server-only";
import type { EnvironmentRef, MachineId } from "@ployz/sdk";
import { Effect, Schema } from "effect";
import { Uuid } from "#/lib/schema";
import { gateOperation } from "#/modules/approvals/approvals.server";
import type { Caller } from "#/modules/identity/actor";
import { machineRemoveAttemptConsuming } from "#/modules/machines/machine-removal.repository";
import { startMachineRemove } from "#/modules/machines/machine-removal.server";
import { requestNamespaceCleanup } from "#/modules/machines/namespace-cleanup.server";
import { requestCliServerDrain } from "#/modules/machines/server-drain.server";
import { planClean, planDrain, planRemove } from "#/modules/machines/server-operations.server";
import { admitOperation, type Admission } from "#/modules/operations/keyed.server";
import type { DataLossIdentity } from "#/modules/runtime/data-loss-identity";
import { planCliServerUpgrade } from "#/modules/server-upgrade/server-upgrade.server";
import type { VolumeRunInput } from "#/modules/volume-run/volume-run";
import { insertRun, planVolumeRun } from "#/modules/volume-run/volume-run.server";

/** The request key and approval a call carries; neither is part of what the key stands for. */
export type OperationCall = { readonly key: string | null; readonly approvalId: string | null };

const refused = (refusal: Extract<Admission, { ok: false }>["refusal"]) => ({ ok: false, refusal }) as const;
const actor = (caller: Caller) => ({ userId: caller.userId, organizationId: caller.organization.id });

/** Null when no Server of the Organization has `machineId`. */
export const drainServer = (caller: Caller, machineId: MachineId, call: OperationCall) =>
  Effect.gen(function* () {
    const plan = yield* planDrain(caller.organization.id, machineId);
    if (plan === null) return null;
    return yield* admitOperation(caller.organization.id, { tool: "server_drain", key: call.key, body: { machineId } }, Effect.gen(function* () {
      const gated = yield* gateOperation(caller, call.approvalId, plan);
      if (!gated.ok) return refused(gated.refusal);
      return (attemptId: string) => requestCliServerDrain(actor(caller), { attemptId, machineId, targets: plan.targets, approvalId: gated.approvalId })
        .pipe(Effect.map((id) => ({ ok: true, id }) as const));
    }));
  });

export const upgradeServer = (caller: Caller, input: { readonly machineId: string; readonly channel: string | undefined }, call: OperationCall) =>
  admitOperation(caller.organization.id, { tool: "server_upgrade", key: call.key, body: input }, planCliServerUpgrade(actor(caller), input));

export const cleanNamespace = (caller: Caller, namespace: string, call: OperationCall) =>
  admitOperation(caller.organization.id, { tool: "server_clean", key: call.key, body: { namespace } }, Effect.gen(function* () {
    const plan = yield* planClean(caller.organization.id, namespace);
    const gated = yield* gateOperation(caller, call.approvalId, plan);
    if (!gated.ok) return refused(gated.refusal);
    return (attemptId: string) => requestNamespaceCleanup(actor(caller), {
      attemptId,
      namespace,
      confirmDataLoss: plan.doomed.map(({ identity }) => identity),
      approvalId: gated.approvalId,
    }).pipe(Effect.map((id) => ({ ok: true, id }) as const));
  }));

/** `confirmed` is the Data Loss the caller accepted, or null for a removal without reset. Null when no Server has `machineId`. */
export const removeServer = (caller: Caller, machineId: string, confirmed: readonly DataLossIdentity[] | null, call: OperationCall) =>
  Effect.gen(function* () {
    const noReset = confirmed === null;
    const consumed = Schema.is(Uuid)(call.approvalId)
      ? yield* machineRemoveAttemptConsuming(caller.organization.id, call.approvalId, machineId, noReset)
      : null;
    if (consumed !== null) return { ok: true, id: consumed } as const;
    const plan = yield* planRemove(caller.organization.id, machineId, confirmed);
    if (plan === null) return null;
    return yield* admitOperation(caller.organization.id, { tool: "server_remove", key: call.key, body: { machineId, confirmed } }, Effect.gen(function* () {
      const gated = yield* gateOperation(caller, call.approvalId, plan);
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

const VOLUME_TOOLS = {
  mirror: "volume_mirror",
  sync: "volume_mirror",
  move: "volume_move",
  release: "volume_release",
  delete_mirror: "volume_mirror_remove",
} as const;

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
      const gated = yield* gateOperation(caller, call.approvalId, planned.asked);
      if (!gated.ok) return refused(gated.refusal);
    }
    return (runId: string) => insertRun(actor(caller), input, planned.volume, runId)
      .pipe(Effect.map((inserted) => inserted.ok ? { ok: true, id: inserted.run.id } as const : inserted));
  }));
