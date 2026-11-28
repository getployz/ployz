import type { MachineId } from "@ployz/sdk";
import { Schema } from "effect";
import {
  dataLossIdentitySchema,
  type DataLossIdentity,
} from "#/modules/runtime/data-loss-identity";
import { requirements, ServerLacks, serverSupports } from "#/modules/runtime/daemon-compat";
import { rustMachineIdSchema } from "#/lib/schema";
import type { MachineRemoveAttemptState, MachineRemoveResult } from "#/modules/machines/machine-removal-states";

export const LoadMachineDataLossInput = Schema.Struct({
  organizationSlug: Schema.NonEmptyString,
  machineId: rustMachineIdSchema,
});

export type LoadMachineDataLossInput = typeof LoadMachineDataLossInput.Type;

export const EnqueueMachineRemoveInput = Schema.Struct({
  organizationSlug: Schema.NonEmptyString,
  machineId: rustMachineIdSchema,
  confirmDataLoss: Schema.Array(dataLossIdentitySchema),
});

export type EnqueueMachineRemoveInput = typeof EnqueueMachineRemoveInput.Type;

/** Forget Servers: the Organization whose every Server is gone. */
export const ForgetServersInput = Schema.Struct({ organizationSlug: Schema.String });

export const ForgetServerInput = Schema.Struct({
  organizationSlug: Schema.NonEmptyString,
  machineId: rustMachineIdSchema,
  requestId: Schema.String.check(Schema.isUUID()),
});
/** Forget one Server, as a removal does. `requestId` keys the request. */
export type ForgetServerInput = typeof ForgetServerInput.Type;

export const GetMachineRemoveAttemptInput = Schema.Struct({
  organizationSlug: Schema.NonEmptyString,
  attemptId: Schema.String.check(Schema.isUUID()),
});

export type GetMachineRemoveAttemptInput =
  typeof GetMachineRemoveAttemptInput.Type;

export type MachineRemoveAttemptView =
  | {
      id: string;
      machineId: string;
      state: "pending" | "running";
    }
  | {
      id: string;
      machineId: string;
      state: "succeeded";
      result: MachineRemoveResult;
    }
  | {
      id: string;
      machineId: string;
      state: "missing_identities";
      missingIdentities: DataLossIdentity[];
    }
  | {
      id: string;
      machineId: string;
      state: "failed" | "cancelled";
      failureMessage: string;
    };

export function toMachineRemoveAttemptView(attempt: {
  id: string;
  machineId: string;
  state: MachineRemoveAttemptState;
  missingIdentities: DataLossIdentity[] | null;
  failureCode: string | null;
  failureMessage: string | null;
  result: MachineRemoveResult | null;
}): MachineRemoveAttemptView {
  switch (attempt.state) {
    case "pending":
    case "running":
      return {
        id: attempt.id,
        machineId: attempt.machineId,
        state: attempt.state,
      };
    case "succeeded":
      if (attempt.result === null) throw new Error("succeeded attempt has no result.");
      return {
        id: attempt.id,
        machineId: attempt.machineId,
        state: attempt.state,
        result: attempt.result,
      };
    case "missing_identities": {
      if (
        attempt.missingIdentities === null ||
        attempt.missingIdentities.length === 0
      ) {
        throw new Error("missing_identities attempt has no identities.");
      }
      return {
        id: attempt.id,
        machineId: attempt.machineId,
        state: "missing_identities",
        missingIdentities: attempt.missingIdentities,
      };
    }
    case "failed":
    case "cancelled": {
      if (attempt.failureMessage === null || attempt.failureMessage === "") {
        throw new Error(`${attempt.state} attempt has no failure message.`);
      }
      return {
        id: attempt.id,
        machineId: attempt.machineId,
        state: attempt.state,
        failureMessage: attempt.failureMessage,
      };
    }
    default: {
      const _exhaustive: never = attempt.state;
      throw new Error(`Unhandled machine remove state: ${_exhaustive}`);
    }
  }
}

export type MachineRemoveAttemptContext = {
  id: string;
  organizationId: string;
  machineId: MachineId;
  state: MachineRemoveAttemptState;
  inngestRunId: string | null;
  confirmDataLoss: DataLossIdentity[];
};

export type MachineRemoveCompletion =
  | { state: "succeeded"; result: MachineRemoveResult }
  | {
      state: "failed" | "cancelled";
      failureCode: string;
      failureMessage: string;
    }
  | { state: "missing_identities"; identities: DataLossIdentity[] };

export type RemoveMachineOutcome =
  /** Taken out of the Cluster; `last` when it was the last Server, which no other forgets, so the Cluster goes. */
  | { kind: "removed"; last: boolean }
  | { kind: "refused"; failureCode: "conflict" | "forbidden" | "server_lacks"; message: string }
  | { kind: "missing_identities"; identities: DataLossIdentity[] };

type RemovalServer = { readonly id: string; readonly capabilities: readonly string[] };

/**
 * The Server whose record lacks its removal step. Nothing is sent to the target; the entry forgets the target's row,
 * and Cloud can't tell ahead which Server it dials, so each one counts.
 */
export function removalLack(
  servers: readonly RemovalServer[],
  removal: { readonly target: string; readonly entries: readonly string[] },
): ServerLacks | null {
  for (const machineId of removal.entries) {
    if (machineId === removal.target) continue;
    const server = servers.find((candidate) => candidate.id === machineId);
    if (server !== undefined && serverSupports(server, requirements.forget) === false) {
      return new ServerLacks({ machineId, capability: requirements.forget.capability });
    }
  }
  return null;
}
