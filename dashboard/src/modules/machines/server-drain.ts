import type { DrainReport } from "@ployz/sdk";
import { Schema } from "effect";
import { machineIdStringSchema } from "#/modules/machines/enrollment";

/**
 * One Drain Cloud runs on one Server, as its row moves: the click writes `pending`, the run claims it `running` right
 * before the Engine is asked, and the Engine's report ends it `finished`. The rest end it without a report, each under
 * an end code that decides its state (`DRAIN_END_CODES`).
 */
export const DRAIN_STATES = ["pending", "running", "finished", "failed", "cancelled", "unknown"] as const;
export type DrainState = (typeof DRAIN_STATES)[number];

/** How a Drain ended without a report, and the state each end leaves its row in. */
export const DRAIN_END_CODES = {
  /** The Engine said no before anything moved: the Server wasn't found, or Cloud couldn't reach the cluster. */
  refused: "failed",
  /** It never asked the Engine: the request couldn't be queued, no run picked it up in time, or its run failed first. */
  not_started: "failed",
  /** Its run was cancelled before it asked the Engine. */
  cancelled: "cancelled",
  /** Its run was cancelled while the Engine worked: it may have moved anything. */
  interrupted: "unknown",
  /** Cloud lost track of a Drain it had started, or got no answer in a day: it may have moved anything. */
  lost: "unknown",
} as const satisfies Record<string, Exclude<DrainState, "pending" | "running" | "finished">>;
export type DrainEndCode = keyof typeof DRAIN_END_CODES;
/** An end Cloud names on its own; only the Engine's refusal carries words. */
export type DrainEndCodeWithoutMessage = Exclude<DrainEndCode, "refused">;

/** A request no run picked up in this long never starts; the hourly sweep closes it. */
export const DRAIN_PENDING_LIMIT_MS = 15 * 60_000;
/** A Drain running this long lost its run; the hourly sweep closes it as lost. */
export const DRAIN_RUNNING_LIMIT_MS = 24 * 60 * 60_000;

export const RequestServerDrainInput = Schema.Struct({
  organizationSlug: Schema.String.check(Schema.isNonEmpty()),
  machineId: machineIdStringSchema,
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
  | { readonly attemptId: string; readonly state: "pending"; readonly requestedAt: string }
  | { readonly attemptId: string; readonly state: "running"; readonly startedAt: string }
  | { readonly attemptId: string; readonly state: "finished"; readonly endedAt: string; readonly report: DrainReport }
  | EndedDrain;

/** Every Server's latest Drain, by Machine ID; a Server never drained has none. */
export type LatestDrains = Readonly<Record<string, LatestDrain>>;
