import type { PrShutdown } from "#/modules/pr-environments/tables";

/** One Destination of a PR Environment, as the review has it: its changes, and its save there, if any. */
type Landing = { destination: { id: string }; rows: readonly unknown[]; saved: unknown };

export type BottomBarState<L extends Landing, P extends { closed: boolean }> = {
  /** A starting point's name. */
  startingPoint: string | null;
  staged: boolean;
  /** A running or queued attempt whose Deployment Page isn't open. */
  attempt: string | null;
  shutdown: PrShutdown | null;
  waiting: boolean;
  /** Null on a root. A PR Environment's `pullRequest` saves into each of `goesTo`; any other Branch saves `changes` into its Parent. */
  branch: { changes: number; updates: number; pullRequest: P | null; goesTo: L[] } | null;
};

/**
 * What the Environment itself has, first that applies: a starting point, changes to deploy, a running attempt, a shutdown
 * (running, Off or failed), then changes that go live with a pull request.
 */
export type OwnRow = { kind: "starting_point"; name: string } | { kind: "staged" } | { kind: "attempt"; deploymentId: string }
  | { kind: "shutdown"; shutdown: PrShutdown } | { kind: "waiting" };

/** What a Branch has for a Destination: changes to save, changes saved to go live with its pull request, else updates from its Parent. */
export type ParentRow<L extends Landing, P> = { kind: "save"; into: { landing: L; pullRequest: P } | null }
  | { kind: "saved"; landing: L; saved: NonNullable<L["saved"]>; pullRequest: P } | { kind: "update" };
export type SaveRow<L extends Landing, P> = Extract<ParentRow<L, P>, { kind: "save" }>

/**
 * The bottom bar's rows. Row 1 is the Environment itself; on a Branch, the rows after it are what it has for its
 * Destinations, so Save is always one tap away: one for its Parent, or one per Destination of a PR Environment. A root
 * has one row. Either is absent when it has nothing.
 */
export function bottomBarRows<L extends Landing, P extends { closed: boolean }>(state: BottomBarState<L, P>) {
  const own: OwnRow | null = state.startingPoint !== null ? { kind: "starting_point", name: state.startingPoint }
    : state.staged ? { kind: "staged" }
    : state.attempt !== null ? { kind: "attempt", deploymentId: state.attempt }
    : state.shutdown ? { kind: "shutdown", shutdown: state.shutdown }
    : state.waiting ? { kind: "waiting" } : null;
  const branch = state.branch;
  if (!branch) return { own, parent: [] };
  const pullRequest = branch.pullRequest;
  // A closed pull request takes no more saves.
  const destinations: ParentRow<L, P>[] = !pullRequest ? branch.changes > 0 ? [{ kind: "save", into: null }] : []
    : pullRequest.closed ? []
    : branch.goesTo.flatMap((landing): ParentRow<L, P>[] => landing.saved ? [{ kind: "saved", landing, saved: landing.saved, pullRequest }]
      : landing.rows.length > 0 ? [{ kind: "save", into: { landing, pullRequest } }] : []);
  return { own, parent: destinations.length || branch.updates === 0 ? destinations : [{ kind: "update" as const }] };
}
