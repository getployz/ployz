/** What the Environment itself has, first that applies: a starting point, changes to deploy, a running attempt, then changes that go live with a pull request. */
export type OwnRow = "starting_point" | "staged" | "attempt" | "waiting";
/** What a Branch has for a Destination: changes to save, changes saved to go live with its pull request, else updates from its Parent. */
export type ParentRow = "save" | "saved" | "update";

type Parent = { row: ParentRow; destination: number | null };

export type BottomBarState = {
  startingPoint: boolean;
  staged: boolean;
  /** A running or queued attempt whose Deployment Page isn't open. */
  attempt: boolean;
  waiting: boolean;
  /**
   * Null on a root. A PR Environment has one entry in `destinations` per Destination, each with its own row; any other
   * Branch saves `changes` into its Parent.
   */
  branch: { changes: number; updates: number; destinations: Array<{ changes: number; saved: boolean }> | null } | null;
};

/**
 * The bottom bar's rows. Row 1 is the Environment itself; on a Branch, the rows after it are what it has for its
 * Destinations, so Save is always one tap away: one for its Parent, or one per Destination of a PR Environment (by
 * index). A root has one row. Either is absent when it has nothing.
 */
export function bottomBarRows(state: BottomBarState): { own: OwnRow | null; parent: Parent[] } {
  const own: OwnRow | null = state.startingPoint ? "starting_point" : state.staged ? "staged" : state.attempt ? "attempt" : state.waiting ? "waiting" : null;
  const branch = state.branch;
  if (!branch) return { own, parent: [] };
  const destinations: Parent[] = branch.destinations
    ? branch.destinations.flatMap((destination, index): Parent[] => destination.saved ? [{ row: "saved", destination: index }]
      : destination.changes > 0 ? [{ row: "save", destination: index }] : [])
    : branch.changes > 0 ? [{ row: "save", destination: null }] : [];
  return { own, parent: destinations.length || branch.updates === 0 ? destinations : [{ row: "update", destination: null }] };
}
