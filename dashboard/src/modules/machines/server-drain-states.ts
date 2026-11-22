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

/**
 * A Drain that ended without a report, in words: `ui` on the Server page, `cli` for `ployz` and history. A cancelled
 * Drain says when on the page, so it has no page line; a refusal's CLI words are the Engine's own.
 */
export const DRAIN_END_WORDS = {
  refused: { ui: "Drain didn't start", cli: null },
  not_started: { ui: "Drain didn't start. Try again.", cli: "The drain never started." },
  cancelled: { ui: null, cli: "The drain was cancelled before it started." },
  interrupted: {
    ui: "Drain was cancelled. Running here shows what's still on this server.",
    cli: "The drain was cancelled while it ran; some services may have moved.",
  },
  lost: {
    ui: "Drain didn't finish. Running here shows what's still on this server.",
    cli: "Cloud lost track of the drain; some services may have moved.",
  },
} as const satisfies Record<DrainEndCode, { readonly ui: string | null; readonly cli: string | null }>;
