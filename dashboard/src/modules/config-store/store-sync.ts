import type { BranchView, DeploymentSummary, RowId, SyncRow, SyncView } from "@ployz/sdk";
import type { NeverSyncing } from "./store-pending";
import { plural } from "#/lib/plural";
import { rowText, settingName, sourceChange } from "./store-branches";

/** One change of the Sync dialog in words: its name (a variable's is its key, in monospace) and at most one badge. */
export type SyncLine = {
  name: string;
  variable: boolean;
  badge: string | null;
  /** The receiver's value now, and the one that lands; a secret lands without one. */
  before: string;
  after: string;
};

export function syncLine(row: SyncRow, into: string): SyncLine {
  const whole = row.name === null;
  const [before, after] = whole || row.secret ? ["", ""]
    : row.name === "source" ? sourceChange(row.into, row.from) : [rowText(row.into, row.name), rowText(row.from, row.name)];
  return {
    // A whole node is named by its kind; its section names it.
    ...row.name === null ? { name: row.kind === "volume" ? "Volume" : "Service", variable: false } : settingName(row.name),
    // A secret's value never syncs, whatever else holds.
    badge: row.held_by !== null ? `Included with ${row.held_by}` : row.secret ? "Secret" : row.change === "conflict" ? `Changed in ${into}` : row.change === "new" ? "New" : null,
    before,
    after,
  };
}

/** The rows by node, in the Store's order: a section each. */
export function syncSections(rows: readonly SyncRow[]) {
  const sections = new Map<string, Pick<SyncRow, "node" | "kind"> & { rows: SyncRow[] }>();
  for (const row of rows) {
    const section = sections.get(row.node) ?? { node: row.node, kind: row.kind, rows: [] };
    section.rows.push(row);
    sections.set(row.node, section);
  }
  return [...sections.values()];
}

/** A row as the user reviewed it: a flip holds only while the row still reads the same. */
export function reviewed(row: SyncRow) {
  return JSON.stringify([row.ticked, row.change, row.from, row.into]);
}

/** The rows the user flipped away from their default, each as they reviewed it. */
export type Flips = ReadonlyMap<RowId, string>;

/** Flips `row` back, or away from its default as it reads now. */
export function flipRow(flips: Flips, row: SyncRow): Flips {
  const next = new Map(flips);
  if (next.get(row.row) === reviewed(row)) next.delete(row.row);
  else next.set(row.row, reviewed(row));
  return next;
}

/** Whether a row the user flipped changed since: it is back at its default. */
export function flipsMoved(rows: readonly SyncRow[], flips: Flips) {
  return rows.some((row) => flips.has(row.row) && flips.get(row.row) !== reviewed(row));
}

/**
 * What a Sync carries, from the rows the user flipped away from their default (`flips`). A row `requires` its new
 * node: leaving the node out leaves it out; ticked, each can still be left out on its own.
 */
export function syncPicks(rows: readonly SyncRow[], flips: Flips) {
  const ticked = (row: SyncRow) => row.ticked !== (flips.get(row.row) === reviewed(row));
  const left = new Set(rows.filter((row) => !ticked(row)).map((row) => row.row));
  return rows.filter((row) => row.held_by === null && ticked(row) && !(row.requires !== null && left.has(row.requires)));
}

/**
 * What the Sync button says, where its Sync goes (`into`) and the changes that carries, and the count it shows, if
 * that's what it says.
 */
export type SyncButtonState = { label: string; into: string; changes: number; count: number | null };

/**
 * The Sync button's words, the first that applies: a shutdown under way or failed, Off, then what a Sync into the
 * Parent carries. A PR Environment shuts down until its next push; any other Branch comes off the Servers to close.
 */
export function syncButtonState(branch: Pick<BranchView, "parent" | "to_parent" | "pull_request">, removal: DeploymentSummary | null): SyncButtonState {
  const shuts = branch.pull_request !== null;
  const [into, changes] = [branch.parent, branch.to_parent];
  const say = (label: string, count: number | null = null) => ({ label, into, changes, count });
  if (removal?.in_flight) return say(shuts ? "Shutting down" : "Closing");
  if (removal?.status === "applied") return say(shuts ? "Off" : "Closing");
  if (removal) return say(shuts ? "Shutdown failed" : "Closing failed");
  return changes > 0 ? say("Sync", changes) : say("In sync");
}

const DAY = 24 * 60 * 60;

/** "Closes in 3 days" from when the Store closes an idle Branch (seconds since the epoch); never less than a day. */
export function closesIn(closesAt: number, now = Date.now() / 1000) {
  return `Closes in ${plural(Math.max(1, Math.ceil((closesAt - now) / DAY)), "day")}`;
}

/**
 * `view` with `pending` Never syncs echoed: a Sync from or into a marking Environment offers its newly marked row no more,
 * and lists it never synced; a row synced again loses that Environment's mark, and with none left waits for the Store to offer it.
 */
export function showNeverSyncing(view: SyncView, pending: readonly NeverSyncing[]): SyncView {
  return pending.reduce((shown, { environment, rows: named, off }) => {
    if (shown.from.name !== environment && shown.into.name !== environment) return shown;
    const rows = new Set(named);
    if (off) {
      return { ...shown, never_synced: shown.never_synced.flatMap((entry) => {
        const marks = entry.marks.filter((mark) => mark.environment !== environment || !rows.has(mark.row));
        return marks.length ? [{ ...entry, marks }] : [];
      }) };
    }
    const marking = shown.rows.filter((row) => rows.has(row.row));
    return {
      ...shown,
      rows: shown.rows.filter((row) => !rows.has(row.row)),
      never_synced: [...shown.never_synced.map((entry) => !rows.has(entry.row) || entry.marks.some((mark) => mark.environment === environment)
        ? entry : { ...entry, marks: [...entry.marks, { environment, row: entry.row }] }), ...marking.map(({ row, node, kind, name }) => ({ row, node, kind, name, marks: [{ environment, row }] }))],
    };
  }, view);
}
