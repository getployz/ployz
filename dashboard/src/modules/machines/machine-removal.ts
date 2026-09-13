import { Schema } from "effect";
import {
  dataLossIdentitySchema,
  type DataLossIdentity,
} from "#/modules/runtime/data-loss-identity";

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

export type MachineRemoveAttemptView =
  | {
      id: string;
      machineId: string;
      state: "pending" | "running" | "succeeded";
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
}): MachineRemoveAttemptView {
  switch (attempt.state) {
    case "pending":
    case "running":
    case "succeeded":
      return {
        id: attempt.id,
        machineId: attempt.machineId,
        state: attempt.state,
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
};

export type MachineRemoveCompletion =
  | { state: "succeeded" }
  | {
      state: "failed" | "cancelled";
      failureCode: string;
      failureMessage: string;
    }
  | { state: "missing_identities"; identities: DataLossIdentity[] };

export type RemoveMachineOutcome =
  | { kind: "removed" }
  | { kind: "missing_identities"; identities: DataLossIdentity[] };
