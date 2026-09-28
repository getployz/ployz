/** What the Environment itself has, first that applies: a starting point, changes to deploy, a running attempt, then changes waiting for a pull request. */
export type OwnRow = "starting_point" | "staged" | "attempt" | "waiting";
/** What a Branch has for its Parent: changes to save, else updates from it. A PR Environment's row is its own. */
export type ParentRow = "save" | "update" | "pull_request";

export type BottomBarState = {
  startingPoint: boolean;
  staged: boolean;
  /** A running or queued attempt whose Deployment Page isn't open. */
  attempt: boolean;
  waiting: boolean;
  /** Null on a root. */
  branch: { pullRequest: boolean; changes: number; updates: number } | null;
};

/**
 * The bottom bar's rows. Row 1 is the Environment itself; on a Branch, row 2 is what it has for its Parent, so Save is
 * always one tap away. A root has one row. Either row is absent when it has nothing.
 */
export function bottomBarRows(state: BottomBarState) {
  const own: OwnRow | null = state.startingPoint ? "starting_point" : state.staged ? "staged" : state.attempt ? "attempt" : state.waiting ? "waiting" : null;
  const branch = state.branch;
  const parent: ParentRow | null = !branch || (branch.changes === 0 && branch.updates === 0) ? null
    : branch.pullRequest ? "pull_request" : branch.changes > 0 ? "save" : "update";
  return { own, parent };
}
