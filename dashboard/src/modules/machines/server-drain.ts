import type { DrainReport, MachineRef, MoveFailure, ServiceDrain, StayReason } from "@ployz/sdk";
import { Schema } from "effect";
import { machineIdStringSchema } from "#/modules/machines/enrollment";

/**
 * One Drain Cloud runs on one Server, as its row moves: the click writes `pending`, the run claims it `running` right
 * before the Engine is asked, and the Engine's report ends it `finished`. The rest end it without a report: `failed`
 * (the Engine refused, or the run never got to ask), `cancelled` (the run was cancelled), `unknown` (the run lost
 * track of a Drain it had started: it may have moved anything).
 */
export const DRAIN_STATES = ["pending", "running", "finished", "failed", "cancelled", "unknown"] as const;
export type DrainState = (typeof DRAIN_STATES)[number];
export type DrainEndState = Exclude<DrainState, "pending" | "running">;

/** Why a Drain ended without a report. */
export const DRAIN_FAILURE_CODES = [
  /** The request's event could not be sent: the run never existed. */
  "dispatch_failed",
  /** The Engine said no before anything moved: the Server wasn't found, or Cloud couldn't reach the cluster. */
  "refused",
  /** The run failed for good before it asked the Engine. */
  "workflow_failed",
  /** No run picked the request up in time. */
  "never_started",
  /** The run was cancelled. */
  "cancelled",
  /** The run lost track of a Drain it had started. */
  "lost",
] as const;
export type DrainFailureCode = (typeof DRAIN_FAILURE_CODES)[number];

/** A request no run picked up in this long never starts; the hourly sweep closes it. */
export const DRAIN_PENDING_LIMIT_MS = 15 * 60_000;
/** A Drain running this long lost its run; the hourly sweep closes it as unknown. */
export const DRAIN_RUNNING_LIMIT_MS = 24 * 60 * 60_000;

export const RequestServerDrainInput = Schema.Struct({
  organizationSlug: Schema.String.check(Schema.isNonEmpty()),
  machineId: machineIdStringSchema,
  /** Minted by the confirming tab: the row's id, so sending the same request again returns the same Drain. */
  requestId: Schema.String.check(Schema.isUUID()),
});
export type RequestServerDrainInput = typeof RequestServerDrainInput.Type;

/** One Server's latest Drain as the page reads it: each state with exactly its evidence. */
export type LatestDrain =
  | { readonly attemptId: string; readonly state: "pending"; readonly requestedAt: string }
  | { readonly attemptId: string; readonly state: "running"; readonly startedAt: string }
  | { readonly attemptId: string; readonly state: "finished"; readonly endedAt: string; readonly report: DrainReport }
  | {
    readonly attemptId: string;
    readonly state: "failed" | "cancelled" | "unknown";
    readonly endedAt: string;
    readonly failureCode: DrainFailureCode;
    readonly failureMessage: string;
  };

/** Every Server's latest Drain, by Machine ID; a Server never drained has none. */
export type LatestDrains = Readonly<Record<string, LatestDrain>>;

export const isActiveDrain = (latest: LatestDrain | null | undefined) =>
  latest?.state === "pending" || latest?.state === "running";

export type DrainTone = "moved" | "stopped" | "stayed" | "failed" | "neutral";

/** One Service's line in the inline result. */
export type DrainRow = {
  /** The Qualified Service. */
  readonly key: string;
  readonly name: string;
  readonly namespace: string;
  /** A Global: it runs on every Server, so the Drain stopped it here rather than moving it. */
  readonly global: boolean;
  readonly tone: DrainTone;
  /** The outcome cell: "Moved to web-1", "Stopped here", "Stayed", "Failed", "Nothing to move", "Not attempted". */
  readonly label: string;
  /** Why, in user words, under the name; null shows the Namespace there instead. */
  readonly reason: string | null;
  /** It stayed because its data is on this Server: draining again won't move it. */
  readonly pinned: boolean;
};

export type DrainView =
  /** Never drained: the row says what Drain does. */
  | { readonly kind: "idle" }
  /** This tab asked; the row hasn't arrived yet. */
  | { readonly kind: "starting" }
  /** Requested, and waiting for a run: behind `behind`'s Drain when another Server's is running. */
  | { readonly kind: "queued"; readonly behind: string | null }
  | { readonly kind: "running"; readonly since: string }
  | {
    readonly kind: "finished";
    readonly at: string;
    /** "3 moved, 1 stopped, 2 stayed" | "Everything moved off web-2" | "Nothing was running here". */
    readonly summary: string;
    /** Why it ended early, in user words; null when it handled every Service. */
    readonly stoppedEarly: string | null;
    readonly rows: readonly DrainRow[];
    /** Something stayed, failed or wasn't reached: the button reads Drain again. */
    readonly again: boolean;
  }
  | { readonly kind: "failed"; readonly at: string; readonly words: string; readonly details: string | null }
  | { readonly kind: "cancelled"; readonly at: string }
  | { readonly kind: "unknown"; readonly at: string; readonly words: string };

/** Whether a Drain is under way from this Server's point of view: the button spins and the switch waits. */
export const drainBusy = (view: DrainView) =>
  view.kind === "starting" || view.kind === "queued" || view.kind === "running";

export const drainButtonLabel = (view: DrainView): "Drain" | "Drain again" =>
  (view.kind === "finished" && view.again) || view.kind === "failed" || view.kind === "cancelled" || view.kind === "unknown"
    ? "Drain again"
    : "Drain";

/**
 * What the Drain row shows for `server`. `requested` is the request id this tab minted when it clicked Drain (null
 * when it didn't): until that Drain's row arrives, the row reads as starting.
 */
export function drainView(input: {
  readonly server: { readonly id: string; readonly name: string };
  readonly latest: LatestDrains;
  /** Every Server's name by Machine ID, to name the one whose Drain this one waits behind. */
  readonly serverNames: ReadonlyMap<string, string>;
  readonly requested: string | null;
}): DrainView {
  const { server, latest, requested } = input;
  const own = latest[server.id] ?? null;
  if (requested !== null && own?.attemptId !== requested) return { kind: "starting" };
  if (own === null) return { kind: "idle" };
  switch (own.state) {
    case "pending": {
      // Drains in one Organization run one at a time.
      const behind = Object.entries(latest).find(([id, other]) => id !== server.id && other.state === "running");
      return { kind: "queued", behind: behind === undefined ? null : input.serverNames.get(behind[0]) ?? null };
    }
    case "running":
      return { kind: "running", since: own.startedAt };
    case "finished": {
      const { services, stopped } = own.report;
      return {
        kind: "finished",
        at: own.endedAt,
        summary: drainSummary(own.report, server.name),
        stoppedEarly: stopWords(stopped),
        rows: services.map((entry) => drainRow(entry, server.id)),
        again: stopped !== null || services.some((entry) => !complete(entry)),
      };
    }
    case "failed":
      return { kind: "failed", at: own.endedAt, words: failureCodeWords(own.failureCode), details: own.failureCode === "refused" ? own.failureMessage : null };
    case "cancelled":
      return { kind: "cancelled", at: own.endedAt };
    case "unknown":
      return { kind: "unknown", at: own.endedAt, words: failureCodeWords(own.failureCode) };
  }
}

/** The Drain did everything it could for this Service: moved it, found nothing to move, or stopped a Global here. */
const complete = (entry: ServiceDrain) =>
  entry.result === "moved" || entry.result === "nothing_to_move" || entry.result === "retired";

/** The counts line, in the order the page reads: moved, stopped, stayed, failed, not attempted. */
export function drainSummary(report: DrainReport, serverName: string): string {
  const { services } = report;
  if (services.length === 0) return "Nothing was running here";
  const count = (...results: ReadonlyArray<ServiceDrain["result"]>) =>
    services.filter((entry) => results.includes(entry.result)).length;
  const moved = count("moved");
  const stopped = count("retired");
  if (services.every(complete)) return moved + stopped > 0 ? `Everything moved off ${serverName}` : "Nothing needed to move";
  return [
    moved > 0 ? `${moved} moved` : null,
    stopped > 0 ? `${stopped} stopped` : null,
    count("stays") > 0 ? `${count("stays")} stayed` : null,
    count("failed", "not_retired") > 0 ? `${count("failed", "not_retired")} failed` : null,
    count("not_attempted") > 0 ? `${count("not_attempted")} not attempted` : null,
  ].filter((part) => part !== null).join(", ");
}

/** Where a Service's containers went, by Server name; one name per Server however many containers moved there. */
const destinations = (moves: ReadonlyArray<{ readonly to: MachineRef }>) =>
  [...new Set(moves.map((move) => move.to.name))].join(" and ");

/** One report entry as a row. A `result` this page doesn't know (a newer Engine's) renders neutral, never crashes. */
export function drainRow(entry: ServiceDrain, drained: string): DrainRow {
  const slash = entry.service.indexOf("/");
  const base = {
    key: entry.service,
    name: slash < 0 ? entry.service : entry.service.slice(slash + 1),
    namespace: slash < 0 ? "" : entry.service.slice(0, slash),
    global: false,
    reason: null,
    pinned: false,
  };
  switch (entry.result) {
    case "moved":
      return { ...base, tone: "moved", label: `Moved to ${destinations(entry.moves)}` };
    case "nothing_to_move":
      return { ...base, tone: "neutral", label: "Nothing to move" };
    case "failed": {
      const before = entry.moves.length > 0 ? ` Moved to ${destinations(entry.moves)} before that.` : "";
      return { ...base, tone: "failed", label: "Failed", reason: failureWords(entry.failure, drained) + before };
    }
    case "stays": {
      const reason = entry.reason;
      const pinned = (reason.kind === "volume" || reason.kind === "bind_mount") && reason.server.id === drained;
      return { ...base, tone: "stayed", label: "Stayed", reason: stayWords(reason, drained), pinned };
    }
    case "retired":
      return { ...base, tone: "stopped", label: "Stopped here", reason: "Runs on every server", global: true };
    case "not_retired":
      return { ...base, tone: "failed", label: "Failed", reason: "Couldn't stop it here", global: true };
    case "not_attempted":
      return { ...base, tone: "neutral", label: "Not attempted", reason: "The drain stopped first" };
    default:
      return { ...base, tone: "neutral", label: "Unknown outcome" };
  }
}

/** Why it stayed, in the user's words. The Engine's English is CLI copy; this is Cloud's. */
export function stayWords(reason: StayReason, drained: string): string {
  const at = (server: MachineRef) => server.id === drained ? "this server" : server.name;
  switch (reason.kind) {
    case "volume":
      return `Its volume ${reason.volume} is on ${at(reason.server)}`;
    case "bind_mount":
      return `It uses a folder on ${at(reason.server)}`;
    case "mid_rollout":
      return "A deploy is in progress. Deploy it first.";
    case "eligibility_unknown":
      return `Can't tell yet whether ${reason.server.name} can run it`;
    case "unobserved":
      return `${reason.server.name} didn't answer`;
    case "entry_unobservable":
      return "Lost contact with your servers";
    case "global":
      return "Runs on every server";
    case "no_destination":
      return "No other server can run it";
    default:
      return "It stays here";
  }
}

/** Why a move failed, in the user's words, and where the Service still runs. */
export function failureWords(failure: MoveFailure, drained: string): string {
  const stillOn = (from: MachineRef | null) => from === null || from.id === drained ? "It still runs here." : `It still runs on ${from.name}.`;
  switch (failure.stage) {
    case "no_destination":
      return `No other server could run it. ${stillOn(failure.from)}`;
    case "source_too_old":
      return `Upgrade ${failure.from.name} first, then drain again.`;
    case "read_image":
      return `Couldn't read its image on ${failure.from.name}. ${stillOn(failure.from)}`;
    case "copy_image":
      return `Couldn't copy its image to ${failure.to.name}. ${stillOn(failure.from)}`;
    case "not_serving":
      return `Its new container on ${failure.to.name} didn't become healthy. ${stillOn(failure.from)}`;
    case "old_not_removed":
      return `It now runs on ${failure.to.name} too, but its container ${failure.from.id === drained ? "here" : `on ${failure.from.name}`} couldn't be removed.`;
    case "cancelled":
      return `The drain stopped mid-move. ${stillOn(failure.from)}`;
    case "unobservable":
      return "Lost contact with your servers. It still runs here.";
    case "refused": {
      const why = stayWords(failure.reason, drained);
      return `Stopped moving it. ${why.endsWith(".") ? why : `${why}.`}`;
    }
    default:
      return "It still runs here.";
  }
}

/** Why the Drain ended before handling every Service. Cloud cancels a Drain only by closing its session. */
export function stopWords(stopped: DrainReport["stopped"]): string | null {
  if (stopped === null) return null;
  switch (stopped.kind) {
    case "cancelled":
      return "Cloud's connection to your servers closed";
    case "entry_unreachable":
      return "Lost contact with your servers";
    default:
      return "The drain stopped early";
  }
}

/** A Drain that ended without a report. */
export function failureCodeWords(code: DrainFailureCode): string {
  switch (code) {
    case "refused":
      return "Drain didn't start";
    case "dispatch_failed":
    case "never_started":
    case "workflow_failed":
      return "Drain didn't start. Try again.";
    case "cancelled":
      return "Drain was cancelled. Running here shows what's still on this server.";
    case "lost":
      return "Drain didn't finish. Running here shows what's still on this server.";
  }
}

/** The confirm dialog's list: what runs here, by name, minus Namespaces no Project owns (a Drain leaves those). */
export function drainDialogNames(
  services: ReadonlyArray<{ readonly name: string; readonly namespace: string | null }>,
  strays: ReadonlySet<string>,
): string[] {
  return [...new Set(services
    .filter((service) => service.namespace === null || !strays.has(service.namespace))
    .map((service) => service.name))];
}

/** What the Remove server row says about what still runs here. */
export type RemoveHint =
  | { readonly kind: "none" }
  /** Drain first: it would move them. */
  | { readonly kind: "drain"; readonly count: number }
  /** The latest Drain left these here because their data is on this Server: draining again won't move them. */
  | { readonly kind: "pinned"; readonly names: readonly string[] };

/** The hint under Remove server, from what runs here (the Runtime watch) and the latest Drain. Quiet while one runs. */
export function removeHint(view: DrainView, running: ReadonlyArray<{ readonly identity: string; readonly name: string }>): RemoveHint {
  if (running.length === 0 || drainBusy(view)) return { kind: "none" };
  const pinned = new Set(view.kind === "finished" ? view.rows.filter((row) => row.pinned).map((row) => row.key) : []);
  if (running.every((service) => pinned.has(service.identity))) {
    return { kind: "pinned", names: [...new Set(running.map((service) => service.name))] };
  }
  return { kind: "drain", count: running.length };
}
