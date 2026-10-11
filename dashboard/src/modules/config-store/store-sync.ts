import type { BranchView, DeploymentSummary, RowId, SyncRow } from "@ployz/sdk";
import { plural } from "#/lib/plural";
import { rowText, settingName } from "./store-branches";

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
  return {
    // A whole node is named by its kind; its section names it.
    ...row.name === null ? { name: row.kind === "volume" ? "Volume" : "Service", variable: false } : settingName(row.name),
    // A secret's value never syncs, whatever else holds.
    badge: row.held_by !== null ? `Included with ${row.held_by}` : row.secret ? "Secret" : row.change === "conflict" ? `Changed in ${into}` : row.change === "new" ? "New" : null,
    before: whole || row.secret ? "" : rowText(row.into, row.name),
    after: whole || row.secret ? "" : rowText(row.from, row.name),
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
  return changes > 0 ? say(`Sync to ${into}`, changes) : say(`In sync with ${into}`);
}

const DAY = 24 * 60 * 60;

/** "Closes in 3 days" from when the Store closes an idle Branch (seconds since the epoch); never less than a day. */
export function closesIn(closesAt: number, now = Date.now() / 1000) {
  return `Closes in ${plural(Math.max(1, Math.ceil((closesAt - now) / DAY)), "day")}`;
}
