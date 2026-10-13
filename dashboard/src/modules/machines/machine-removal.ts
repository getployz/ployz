import { Schema } from "effect";
import {
  dataLossIdentitySchema,
  type DataLossIdentity,
} from "#/modules/runtime/data-loss-identity";
import { requirements, ServerLacks, serverSupports } from "#/modules/runtime/daemon-compat";

export const MACHINE_REMOVE_ATTEMPT_STATES = [
  "pending",
  "running",
  "succeeded",
  "failed",
  "cancelled",
  "missing_identities",
] as const;

export type MachineRemoveAttemptState =
  (typeof MACHINE_REMOVE_ATTEMPT_STATES)[number];

const NonEmptyString = Schema.String.check(Schema.isNonEmpty());

export const LoadMachineDataLossInput = Schema.Struct({
  organizationSlug: NonEmptyString,
  machineId: NonEmptyString,
});

export type LoadMachineDataLossInput = typeof LoadMachineDataLossInput.Type;

export const EnqueueMachineRemoveInput = Schema.Struct({
  organizationSlug: NonEmptyString,
  machineId: NonEmptyString,
  confirmDataLoss: Schema.Array(dataLossIdentitySchema),
});

export type EnqueueMachineRemoveInput = typeof EnqueueMachineRemoveInput.Type;

export const GetMachineRemoveAttemptInput = Schema.Struct({
  organizationSlug: NonEmptyString,
  attemptId: Schema.String.check(Schema.isUUID()),
});

export type GetMachineRemoveAttemptInput =
  typeof GetMachineRemoveAttemptInput.Type;

/** What became of Cloud's hold once a removed Server left the Cluster. */
export type ServerRelease =
  /** Other Servers keep the Cluster; Cloud dropped only this Server's key. */
  | { kind: "others_remain" }
  /** It was the last: Cloud forgot the Cluster, and the next Server founds a new one. */
  | { kind: "released" }
  /** Cloud kept its hold, for `reason`. */
  | { kind: "kept"; reason: string };

/** A settled removal: why the reset didn't finish, if it didn't, and what became of Cloud's hold. */
export type MachineRemoveResult = { resetWarning: string | null; release: ServerRelease };

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
  machineId: string;
  state: MachineRemoveAttemptState;
  inngestRunId: string | null;
  confirmDataLoss: DataLossIdentity[];
  /** Take the Server out without resetting it: it keeps its state, and Cloud never lets go of the Cluster for it. */
  noReset: boolean;
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
  | { kind: "removed"; resetWarning: string | null }
  /** The Server reset, and its row is still listed: Forget alone finishes the removal, with the Leave's warning. */
  | { kind: "left"; resetWarning: string | null }
  | { kind: "refused"; failureCode: "conflict" | "server_lacks"; message: string }
  | { kind: "missing_identities"; identities: DataLossIdentity[] };

type RemovalServer = { readonly id: string; readonly capabilities: readonly string[] };

/**
 * The Server whose record lacks its removal step. The target leaves unless `noReset`; the entry forgets the target's
 * row, and Cloud can't tell ahead which Server it dials, so each one counts.
 */
export function removalLack(
  servers: readonly RemovalServer[],
  removal: { readonly target: string; readonly entries: readonly string[]; readonly noReset: boolean },
): ServerLacks | null {
  const steps = [
    ...(removal.noReset ? [] : [{ machineId: removal.target, requirement: requirements.leave }]),
    ...removal.entries
      .filter((entry) => entry !== removal.target)
      .map((machineId) => ({ machineId, requirement: requirements.forget })),
  ];
  for (const { machineId, requirement } of steps) {
    const server = servers.find((candidate) => candidate.id === machineId);
    if (server !== undefined && serverSupports(server, requirement) === false) {
      return new ServerLacks({ machineId, capability: requirement.capability });
    }
  }
  return null;
}
