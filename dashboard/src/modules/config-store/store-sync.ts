import type { BranchView, DeploymentSummary, NeverSyncedRow, PullRequestView, SyncRow } from "@ployz/sdk";
import { plural } from "#/lib/plural";
import { settingTitle } from "./catalog";
import { nodeName, rowText } from "./store-branches";

/**
 * The path the Store's other commands (Never sync, Discard) name a Sync row by: `api.env.KEY`, `api.image`,
 * `volumes.data.name`, or the node itself. A row's label names its node as the user does (`data.name`).
 */
export function syncRowPath({ node, label }: Pick<SyncRow | NeverSyncedRow, "node" | "label">) {
  return `${node}${label.slice(nodeName(node).length)}`;
}

/** A row that brings a whole node: its settings land with it. */
export const isWholeNode = (row: Pick<SyncRow, "node" | "label">) => row.label === nodeName(row.node);

/** One change of the Sync dialog in words: its name (a variable's is its key, in monospace) and at most one badge. */
export type SyncLine = {
  row: SyncRow;
  name: string;
  variable: boolean;
  badge: string | null;
  /** The receiver's value now, and the one that lands; a secret lands without one. */
  before: string;
  after: string;
};

/** A change's name within its node: a variable's key, a Setting's title, or the node's kind for a whole node. */
export function syncName(row: Pick<SyncRow | NeverSyncedRow, "node" | "label">) {
  const field = row.label.slice(nodeName(row.node).length + 1);
  const name = isWholeNode(row) ? (row.node.startsWith("volumes.") ? "Volume" : "Service")
    : field.startsWith("env.") ? field.slice("env.".length)
    : field.startsWith("mounts.") ? `Mount of ${field.slice("mounts.".length)}`
    : field === "name" ? "Name" : settingTitle(field) ?? field;
  return { name, variable: field.startsWith("env.") };
}

export function syncLine(row: SyncRow, into: string): SyncLine {
  return {
    row, ...syncName(row),
    badge: row.changed ? `Changed in ${into}` : row.secret ? "Secret" : row.new ? "New" : null,
    before: isWholeNode(row) || row.secret ? "" : rowText(row.into),
    after: isWholeNode(row) || row.secret ? "" : rowText(row.from),
  };
}

/** The rows by node, in the Store's order: a section each. */
export function syncSections(rows: readonly SyncRow[]) {
  const sections = new Map<string, SyncRow[]>();
  for (const row of rows) sections.set(row.node, [...sections.get(row.node) ?? [], row]);
  return [...sections].map(([node, nodeRows]) => ({ node, rows: nodeRows }));
}

/**
 * What a Sync carries, from the rows the user flipped away from their default (`flipped`, by key). A setting of a new
 * node goes with it: it can't be left out on its own, and leaving the node out leaves it out.
 */
export function syncPicks(rows: readonly SyncRow[], flipped: ReadonlySet<string>) {
  const ticked = (row: SyncRow) => row.ticked !== flipped.has(row.key);
  const wholes = new Map(rows.filter(isWholeNode).map((row) => [row.node, ticked(row)]));
  return rows.filter((row) => wholes.get(row.node) ?? ticked(row));
}

/** What undoing a Sync discards in the receiver: each synced setting, or a new node whole. */
export function undoPaths(synced: readonly SyncRow[]) {
  const wholes = new Set(synced.filter(isWholeNode).map((row) => row.node));
  return [...new Set(synced.flatMap((row) => wholes.has(row.node) && !isWholeNode(row) ? [] : [syncRowPath(row)]))];
}

/**
 * A PR Environment's Destination, as its pull request's view has it: where a Sync goes live at the merge (#`number`),
 * how many changes it would hold, and whether one stands there. The first Destination, or the one a Sync stands in.
 */
export type MergeSync = { number: number; into: string; changes: number; standing: boolean };

export function mergeSync(view: PullRequestView, environment: string): MergeSync | null {
  const destinations = view.environments.find((row) => row.environment.name === environment)?.destinations ?? [];
  const destination = destinations.find((row) => row.conditional_sync?.standing) ?? destinations[0];
  if (!view.pull_request || !destination) return null;
  const standing = destination.conditional_sync?.standing === true;
  return { number: view.pull_request.number, into: destination.name, changes: destination.changes, standing };
}

/** What the Sync button says, and the count of changes its Sync carries, if that's what it says. */
export type SyncButtonState = { label: string; count: number | null };

/**
 * The Sync button's words, the first that applies: a shutdown under way or failed, Off, a Conditional Sync standing,
 * then what a Sync into the Parent (a PR Environment's Destination, `merge`) carries. A PR Environment shuts down until
 * its next push; any other Branch comes off the Servers to close.
 */
export function syncButtonState(branch: Pick<BranchView, "parent" | "to_parent" | "pull_request">, removal: DeploymentSummary | null,
  merge: MergeSync | null = null): SyncButtonState {
  const shuts = branch.pull_request !== null;
  if (removal?.in_flight) return { label: shuts ? "Shutting down" : "Closing", count: null };
  if (removal?.status === "applied") return { label: shuts ? "Off" : "Closing", count: null };
  if (removal) return { label: shuts ? "Shutdown failed" : "Closing failed", count: null };
  if (merge?.standing) return { label: `Goes live with #${merge.number}`, count: null };
  const [into, count] = merge ? [merge.into, merge.changes] : [branch.parent, branch.to_parent];
  return count > 0 ? { label: `Sync to ${into}`, count } : { label: `In sync with ${into}`, count: null };
}

/** "3 changes go live in production when #142 merges". */
export function goesLive(changes: number, into: string, number: number) {
  return `${plural(changes, "change")} ${changes === 1 ? "goes" : "go"} live in ${into} when #${number} merges`;
}

const DAY = 24 * 60 * 60;

/** "Closes in 3 days" from when the Store closes an idle Branch (seconds since the epoch); never less than a day. */
export function closesIn(closesAt: number, now = Date.now() / 1000) {
  return `Closes in ${plural(Math.max(1, Math.ceil((closesAt - now) / DAY)), "day")}`;
}
