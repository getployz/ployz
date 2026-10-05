import type { DrainReport, MachineRef, MoveFailure, ServiceDrain, StayReason } from "@ployz/sdk";
import type { DrainEndCode, LatestDrains } from "#/modules/machines/server-drain";
import { splitQualifiedService } from "#/modules/machines/server-services";

export type DrainTone = "moved" | "stopped" | "stayed" | "failed" | "neutral";

/** One Service's line in the inline result. */
export type DrainRow = {
  /** The Qualified Service. */
  readonly key: string;
  readonly name: string;
  readonly namespace: string | null;
  /** A Global: it runs on every Server, so the Drain stopped it here rather than moving it. */
  readonly global: boolean;
  readonly tone: DrainTone;
  /** The outcome cell: "Moved to web-1", "Stopped here", "Stayed", "Failed", "Interrupted", "Nothing to move", "Not attempted". */
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
    /** "3 moved, 1 stopped, 2 stayed" | "Everything moved off web-2" | "Nothing was running here", and whether it
     * couldn't check what's left. */
    readonly summary: string;
    /** Why it ended early, in user words; null when it handled every Service. */
    readonly stoppedEarly: string | null;
    readonly rows: readonly DrainRow[];
    /** Something stayed, failed or wasn't reached, or what's left went unchecked: the button reads Drain again. */
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
        again: !own.report.complete,
      };
    }
    case "failed":
      return { kind: "failed", at: own.endedAt, words: endWords(own.endCode), details: own.endCode === "refused" ? own.refusalMessage : null };
    case "cancelled":
      return { kind: "cancelled", at: own.endedAt };
    case "unknown":
      return { kind: "unknown", at: own.endedAt, words: endWords(own.endCode) };
  }
}

/**
 * The counts line, in the order the page reads: moved, stopped, stayed, failed, not attempted, then what the Drain left
 * alone (no Project owns it, so it never chose it) and whether it couldn't check what's left.
 */
export function drainSummary(report: DrainReport, serverName: string): string {
  const { services, remaining } = report;
  const count = (...results: ReadonlyArray<ServiceDrain["result"]>) =>
    services.filter((entry) => results.includes(entry.result)).length;
  const leftAlone = remaining.kind === "observed" ? remaining.unchosen.length : 0;
  if (report.complete && leftAlone === 0) {
    if (services.length === 0) return "Nothing was running here";
    return count("moved", "retired") > 0 ? `Everything moved off ${serverName}` : "Nothing needed to move";
  }
  const parts = [
    count("moved") > 0 ? `${count("moved")} moved` : null,
    count("retired") > 0 ? `${count("retired")} stopped` : null,
    count("stays") > 0 ? `${count("stays")} stayed` : null,
    count("failed", "not_retired") > 0 ? `${count("failed", "not_retired")} failed` : null,
    count("interrupted") > 0 ? `${count("interrupted")} interrupted` : null,
    count("not_attempted") > 0 ? `${count("not_attempted")} not attempted` : null,
    leftAlone > 0 ? `${leftAlone} left alone` : null,
  ].filter((part) => part !== null).join(", ");
  if (remaining.kind === "observed") return parts;
  return parts === "" ? `Couldn't check what's left on ${serverName}` : `${parts}, but couldn't check what's left`;
}

/** Where a Service's containers went, by Server name; one name per Server however many containers moved there. */
const destinations = (moves: ReadonlyArray<{ readonly to: MachineRef }>) =>
  [...new Set(moves.map((move) => move.to.name))].join(" and ");

/** The moves made before a Service's drain stopped, as a trailing sentence; empty when none were. */
const movedBefore = (moves: ReadonlyArray<{ readonly to: MachineRef }>) =>
  moves.length > 0 ? ` Moved to ${destinations(moves)} before that.` : "";

/** One report entry as a row. A `result` this page doesn't know (a newer Engine's) renders neutral, never crashes. */
export function drainRow(entry: ServiceDrain, drained: string): DrainRow {
  const base = {
    key: entry.service,
    ...splitQualifiedService(entry.service),
    global: false,
    reason: null,
    pinned: false,
  };
  switch (entry.result) {
    case "moved":
      return { ...base, tone: "moved", label: `Moved to ${destinations(entry.moves)}` };
    case "nothing_to_move":
      return { ...base, tone: "neutral", label: "Nothing to move" };
    case "failed":
      return { ...base, tone: "failed", label: "Failed", reason: failureWords(entry.failure, drained) + movedBefore(entry.moves) };
    case "stays": {
      const reason = entry.reason;
      const pinned = (reason.kind === "volume" || reason.kind === "bind_mount") && reason.server.id === drained;
      return { ...base, tone: "stayed", label: "Stayed", reason: stayWords(reason, drained), pinned };
    }
    case "retired":
      return { ...base, tone: "stopped", label: "Stopped here", reason: "Runs on every server", global: true };
    case "not_retired":
      return { ...base, tone: "failed", label: "Failed", reason: "Couldn't stop it here", global: true };
    case "interrupted": {
      const reason = entry.moves.length === 0
        ? "The drain stopped before moving it. It still runs here."
        : `The drain stopped while moving it.${movedBefore(entry.moves)}`;
      return { ...base, tone: "neutral", label: "Interrupted", reason };
    }
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
      return `Its volume is on ${at(reason.server)}`;
    case "bind_mount":
      return `It uses a folder on ${at(reason.server)}`;
    case "mid_rollout":
      return "A deploy is in progress. Deploy it first.";
    case "unobserved":
      return `${reason.server.name} didn't answer`;
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
  const stillOn = (from: MachineRef) => from.id === drained ? "It still runs here." : `It still runs on ${from.name}.`;
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
      return `Its new container on ${failure.to.name} didn't become healthy${failure.replacement_removed ? "" : ", and may still be there"}. ${stillOn(failure.from)}`;
    case "old_not_removed": {
      const here = failure.from.id === drained ? "here" : `on ${failure.from.name}`;
      return failure.old_stopped
        ? `It now runs on ${failure.to.name}. Its container ${here} stopped but couldn't be removed.`
        : `It now runs on ${failure.to.name} too, but its container ${here} couldn't be stopped.`;
    }
    case "cancelled":
      return `The drain stopped mid-move. ${stillOn(failure.from)}${failure.replacement_removed ? "" : ` Its new container on ${failure.to.name} may still be there.`}`;
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

/** A Drain that ended without a report, in the user's words. A cancelled one says when, so it has its own line. */
export function endWords(code: Exclude<DrainEndCode, "cancelled">): string {
  switch (code) {
    case "refused":
      return "Drain didn't start";
    case "not_started":
      return "Drain didn't start. Try again.";
    case "interrupted":
      return "Drain was cancelled. Running here shows what's still on this server.";
    case "lost":
      return "Drain didn't finish. Running here shows what's still on this server.";
  }
}

/** A Service running here as the page lists it. */
type RunningService = { readonly identity: string; readonly name: string; readonly namespace: string | null };

/**
 * What runs here split by whether a Drain acts on it: a Drain leaves Namespaces no Project owns (`strays`) alone. The
 * dialog and the Remove hint both read it, so they agree on what a Drain would move.
 */
export function drainableServices<S extends RunningService>(services: readonly S[], strays: ReadonlySet<string>) {
  const unowned = (service: S) => service.namespace !== null && strays.has(service.namespace);
  return {
    drainable: services.filter((service) => !unowned(service)),
    unowned: services.filter(unowned),
  };
}

const names = (services: ReadonlyArray<{ readonly name: string }>) => [...new Set(services.map((service) => service.name))];

/** One Service in the confirm dialog. `stays` says why a Drain won't move it, when the page knows; null means it moves. */
export type DrainDialogRow = {
  /** The Qualified Service. */
  readonly key: string;
  readonly name: string;
  readonly namespace: string | null;
  /** `data`: the latest Drain left it for its volume or folder here. `unowned`: no Project owns it. */
  readonly stays: "data" | "unowned" | null;
};

/** The confirm dialog's rows: what runs here, the ones a Drain won't move last. */
export function drainDialogRows(view: DrainView, services: readonly RunningService[], strays: ReadonlySet<string>): DrainDialogRow[] {
  const pinned = new Set(view.kind === "finished" ? view.rows.filter((row) => row.pinned).map((row) => row.key) : []);
  const split = drainableServices(services, strays);
  const row = (service: RunningService, stays: DrainDialogRow["stays"]): DrainDialogRow =>
    ({ key: service.identity, name: service.name, namespace: service.namespace, stays });
  const rows = [
    ...split.drainable.map((service) => row(service, pinned.has(service.identity) ? "data" : null)),
    ...split.unowned.map((service) => row(service, "unowned")),
  ];
  return [...rows.filter((entry) => entry.stays === null), ...rows.filter((entry) => entry.stays !== null)];
}

/** What a Drain under way still has to move: what it acts on that runs here now, live from the Runtime watch. */
export function drainLeftRows(running: readonly RunningService[], strays: ReadonlySet<string>): DrainRow[] {
  return drainableServices(running, strays).drainable.map((service) => ({
    key: service.identity,
    name: service.name,
    namespace: service.namespace,
    global: false,
    tone: "neutral",
    label: "Pending",
    reason: null,
    pinned: false,
  }));
}

/** What the Remove server row says about what still runs here. */
export type RemoveHint =
  | { readonly kind: "none"; readonly unowned: readonly string[] }
  /** Drain first: it would move them. */
  | { readonly kind: "drain"; readonly count: number; readonly unowned: readonly string[] }
  /** The latest Drain left these here because their data is on this Server: draining again won't move them. */
  | { readonly kind: "pinned"; readonly names: readonly string[]; readonly unowned: readonly string[] };

/**
 * The hint under Remove server, from what runs here (the Runtime watch) and the latest Drain. It counts only what a
 * Drain would act on, and names apart what it leaves because no Project owns it. Quiet while one runs.
 */
export function removeHint(view: DrainView, running: readonly RunningService[], strays: ReadonlySet<string>): RemoveHint {
  if (drainBusy(view)) return { kind: "none", unowned: [] };
  const split = drainableServices(running, strays);
  const unowned = names(split.unowned);
  if (split.drainable.length === 0) return { kind: "none", unowned };
  const pinned = new Set(view.kind === "finished" ? view.rows.filter((row) => row.pinned).map((row) => row.key) : []);
  if (split.drainable.every((service) => pinned.has(service.identity))) {
    return { kind: "pinned", names: names(split.drainable), unowned };
  }
  return { kind: "drain", count: split.drainable.length, unowned };
}
