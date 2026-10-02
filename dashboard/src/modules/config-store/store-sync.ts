import type { BranchView, DeploymentSummary, PullRequestView, SyncRow } from "@ployz/sdk";
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
  return {
    // A whole node is named by its kind; its section names it.
    ...row.whole ? { name: row.node.startsWith("volumes.") ? "Volume" : "Service", variable: false } : settingName(row.path),
    // A secret's value never syncs, whatever else holds.
    badge: row.secret ? "Secret" : row.changed ? `Changed in ${into}` : row.new ? "New" : null,
    before: row.whole || row.secret ? "" : rowText(row.into),
    after: row.whole || row.secret ? "" : rowText(row.from),
  };
}

/** The rows by node, in the Store's order: a section each. */
export function syncSections(rows: readonly SyncRow[]) {
  const sections = new Map<string, SyncRow[]>();
  for (const row of rows) sections.set(row.node, [...sections.get(row.node) ?? [], row]);
  return [...sections].map(([node, nodeRows]) => ({ node, rows: nodeRows }));
}

/**
 * What a Sync carries, from the rows the user flipped away from their default (`flipped`, by key). Leaving a new node
 * out leaves its settings out; ticked, each of its settings can still be left out on its own.
 */
export function syncPicks(rows: readonly SyncRow[], flipped: ReadonlySet<string>) {
  const ticked = (row: SyncRow) => row.ticked !== flipped.has(row.key);
  const left = new Set(rows.filter((row) => row.whole && !ticked(row)).map((row) => row.node));
  return rows.filter((row) => !left.has(row.node) && ticked(row));
}

/** What undoing a Sync discards in the receiver: each synced setting, or a new node whole. */
export function undoPaths(synced: readonly SyncRow[]) {
  const wholes = new Set(synced.filter((row) => row.whole).map((row) => row.node));
  return [...new Set(synced.flatMap((row) => wholes.has(row.node) && !row.whole ? [] : [row.path]))];
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

/**
 * What the Sync button says, where its Sync goes (`into`) and the changes that carries, and the count it shows, if
 * that's what it says.
 */
export type SyncButtonState = { label: string; into: string; changes: number; count: number | null };

/**
 * The Sync button's words, the first that applies: a shutdown under way or failed, Off, a Conditional Sync standing,
 * then what a Sync into the Parent (a PR Environment's Destination, `merge`) carries. A PR Environment shuts down until
 * its next push; any other Branch comes off the Servers to close.
 */
export function syncButtonState(branch: Pick<BranchView, "parent" | "to_parent" | "pull_request">, removal: DeploymentSummary | null,
  merge: MergeSync | null = null): SyncButtonState {
  const shuts = branch.pull_request !== null;
  const [into, changes] = merge ? [merge.into, merge.changes] : [branch.parent, branch.to_parent];
  const say = (label: string, count: number | null = null) => ({ label, into, changes, count });
  if (removal?.in_flight) return say(shuts ? "Shutting down" : "Closing");
  if (removal?.status === "applied") return say(shuts ? "Off" : "Closing");
  if (removal) return say(shuts ? "Shutdown failed" : "Closing failed");
  if (merge?.standing) return say(`Goes live with #${merge.number}`);
  return changes > 0 ? say(`Sync to ${into}`, changes) : say(`In sync with ${into}`);
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
