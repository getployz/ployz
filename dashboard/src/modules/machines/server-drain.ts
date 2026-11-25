import type { DrainReport } from "@ployz/sdk";
import { Schema } from "effect";
import { rustMachineIdSchema } from "#/lib/schema";
import { DRAIN_END_CODES, type DrainEndCode } from "#/modules/machines/server-drain-states";
import type { VolumeLine } from "#/modules/machines/server-drain-volumes";

/** Each Volume line the Drain's moves give now, by the Service that moves with it. */
export type VolumeLines = Readonly<Record<string, VolumeLine>>;

/** A request no run picked up in this long never starts; the hourly sweep closes it. */
export const DRAIN_PENDING_LIMIT_MS = 15 * 60_000;
/** A Drain running this long lost its run; the hourly sweep closes it as lost. */
export const DRAIN_RUNNING_LIMIT_MS = 24 * 60 * 60_000;

export const RequestServerDrainInput = Schema.Struct({
  organizationSlug: Schema.String.check(Schema.isNonEmpty()),
  machineId: rustMachineIdSchema,
  /** Minted by the confirming tab: the row's id, so sending the same request again returns the same Drain. */
  requestId: Schema.String.check(Schema.isUUID()),
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
  | {
    readonly attemptId: string;
    readonly state: "running";
    readonly startedAt: string;
    /** The Engine's answer once it came, while the Drain moves Volumes; null while the Engine works. */
    readonly report: DrainReport | null;
    readonly lines: VolumeLines;
  }
  | { readonly attemptId: string; readonly state: "finished"; readonly endedAt: string; readonly report: DrainReport; readonly lines: VolumeLines }
  | EndedDrain;

export type RequestedServerDrain = { readonly drain: LatestDrain };

/** Every Server's latest Drain, by Machine ID; a Server never drained has none. */
export type LatestDrains = Readonly<Record<string, LatestDrain>>;
