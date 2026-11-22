import type { DrainReport } from "@ployz/sdk";
import { Schema } from "effect";
import { rustMachineIdSchema } from "#/lib/schema";
import { DRAIN_END_CODES, type DrainEndCode } from "#/modules/machines/server-drain-states";

/** A request no run picked up in this long never starts; the hourly sweep closes it. */
export const DRAIN_PENDING_LIMIT_MS = 15 * 60_000;
/** A Drain running this long lost its run; the hourly sweep closes it as lost. */
export const DRAIN_RUNNING_LIMIT_MS = 24 * 60 * 60_000;

export const RequestServerDrainInput = Schema.Struct({
  organizationSlug: Schema.String.check(Schema.isNonEmpty()),
  machineId: rustMachineIdSchema,
  /** Minted by the confirming tab: the row's id, so sending the same request again returns the same Drain. */
  requestId: Schema.String.check(Schema.isUUID()),
  /** The Services the confirm dialog showed as stopping. A Drain that would now stop another one is refused. */
  stops: Schema.Array(Schema.String),
});
export type RequestServerDrainInput = typeof RequestServerDrainInput.Type;

/** A Drain that ended without a report: its state is the one its code ends in, and only a refusal has words. */
export type EndedDrain = {
  [Code in DrainEndCode]: {
    readonly attemptId: string;
    readonly state: (typeof DRAIN_END_CODES)[Code];
    readonly endedAt: string;
    readonly endCode: Code;
  } & (Code extends "refused" ? { readonly refusalMessage: string } : unknown);
}[DrainEndCode];

/** One Server's latest Drain as the page reads it: each state with exactly its evidence. */
export type LatestDrain =
  | { readonly attemptId: string; readonly state: "pending"; readonly requestedAt: string; readonly serversSilent: boolean }
  | { readonly attemptId: string; readonly state: "running"; readonly startedAt: string }
  | { readonly attemptId: string; readonly state: "finished"; readonly endedAt: string; readonly report: DrainReport }
  | EndedDrain;

/** The Drain a request admitted. */
export type RequestedServerDrain = { readonly drain: LatestDrain };

/** Every Server's latest Drain, by Machine ID; a Server never drained has none. */
export type LatestDrains = Readonly<Record<string, LatestDrain>>;
