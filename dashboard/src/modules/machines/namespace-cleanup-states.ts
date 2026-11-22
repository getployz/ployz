/**
 * One clean of a Namespace no Environment owns, as its row moves: the CLI's request writes `pending`, the run claims it
 * `running` right before the Engine is asked, and the Engine's success ends it `finished`. The rest end it under an end
 * code that decides its state (`CLEANUP_END_CODES`).
 */
export const CLEANUP_STATES = ["pending", "running", "finished", "failed", "cancelled", "unknown"] as const;
export type CleanupState = (typeof CLEANUP_STATES)[number];

/** How a clean ended without finishing, and the state each end leaves its row in. */
export const CLEANUP_END_CODES = {
  /** The Engine said no before removing anything, such as for Volumes the clean didn't preview. */
  refused: "failed",
  /** The Engine removed some of it and failed on the rest. */
  incomplete: "failed",
  /** It never asked the Engine: the request couldn't be queued, no run picked it up in time, or its run failed first. */
  not_started: "failed",
  /** Its run was cancelled before it asked the Engine. */
  cancelled: "cancelled",
  /** Its run was cancelled while the Engine worked: it may have removed anything. */
  interrupted: "unknown",
  /** Cloud lost track of a clean it had started: it may have removed anything. */
  lost: "unknown",
} as const satisfies Record<string, Exclude<CleanupState, "pending" | "running" | "finished">>;
export type CleanupEndCode = keyof typeof CLEANUP_END_CODES;
/** The ends that carry the Engine's words. */
export const CLEANUP_END_CODES_WITH_MESSAGE = ["refused", "incomplete"] as const satisfies readonly CleanupEndCode[];
export type CleanupEndCodeWithoutMessage = Exclude<CleanupEndCode, (typeof CLEANUP_END_CODES_WITH_MESSAGE)[number]>;

/** Why Cloud ended a clean without finishing, for `ployz` and history; the Engine's refusals carry their own words. */
export const CLEANUP_END_WORDS = {
  not_started: "The clean never started.",
  cancelled: "The clean was cancelled before it started.",
  interrupted: "The clean was cancelled while it ran; some Volumes may be gone.",
  lost: "Cloud lost track of the clean; some Volumes may be gone.",
} as const satisfies Record<CleanupEndCodeWithoutMessage, string>;
