import { describe, expect, it } from "vitest";
import { bottomBarRows, type BottomBarState } from "./bottom-bar-rows";

type Landing = { destination: { id: string }; rows: number[]; saved: string | null };
type Pr = { closed: boolean };
const idle: BottomBarState<Landing, Pr> = { startingPoint: null, staged: false, attempt: null, shutdown: null, waiting: false, branch: null };
const branch = (changes: number, updates: number) => ({ changes, updates, pullRequest: null, goesTo: [] });
const open = { closed: false };
const pr = (updates: number, ...destinations: Array<[changes: number, saved: boolean]>) => ({
  changes: 0, updates, pullRequest: open,
  goesTo: destinations.map(([changes, saved], index): Landing => ({ destination: { id: `d${index}` }, rows: Array<number>(changes).fill(0), saved: saved ? `s${index}` : null })),
});
const save = { kind: "save", into: null };
const kinds = (state: BottomBarState<Landing, Pr>) => {
  const rows = bottomBarRows(state);
  return { own: rows.own?.kind ?? null, parent: rows.parent.map((row) => row.kind === "update" ? "update" : `${row.kind}:${row.kind === "saved" ? row.landing.destination.id : row.into?.landing.destination.id ?? "parent"}`) };
};

describe("bottomBarRows", () => {
  it("gives a root one row: the first of changes to deploy, a running attempt, changes going live with a pull request", () => {
    expect(bottomBarRows(idle)).toEqual({ own: null, parent: [] });
    expect(bottomBarRows({ ...idle, staged: true, attempt: "a", waiting: true })).toEqual({ own: { kind: "staged" }, parent: [] });
    expect(bottomBarRows({ ...idle, attempt: "a", waiting: true })).toEqual({ own: { kind: "attempt", deploymentId: "a" }, parent: [] });
    expect(bottomBarRows({ ...idle, waiting: true })).toEqual({ own: { kind: "waiting" }, parent: [] });
  });

  it("puts a starting point before its staged nodes", () => {
    expect(bottomBarRows({ ...idle, startingPoint: "recipe", staged: true, branch: branch(0, 0) })).toEqual({ own: { kind: "starting_point", name: "recipe" }, parent: [] });
  });

  it("gives a Branch a second row for Save in every state of the first", () => {
    for (const own of [
      { startingPoint: "recipe", staged: true }, { staged: true }, { attempt: "a" }, { staged: true, attempt: "a" }, {},
    ]) {
      expect(bottomBarRows({ ...idle, ...own, branch: branch(3, 1) }).parent).toEqual([save]);
    }
    expect(kinds({ ...idle, staged: true, branch: branch(3, 0) })).toEqual({ own: "staged", parent: ["save:parent"] });
    expect(kinds({ ...idle, branch: branch(3, 0) })).toEqual({ own: null, parent: ["save:parent"] });
  });

  it("shows updates from the Parent once nothing is left to save, and no second row when there's neither", () => {
    expect(kinds({ ...idle, attempt: "a", branch: branch(0, 1) })).toEqual({ own: "attempt", parent: ["update"] });
    expect(kinds({ ...idle, staged: true, branch: branch(0, 0) })).toEqual({ own: "staged", parent: [] });
  });

  it("gives a PR Environment a row per Destination with changes: to save, or saved to go live with the pull request", () => {
    expect(kinds({ ...idle, staged: true, branch: pr(0, [2, false]) })).toEqual({ own: "staged", parent: ["save:d0"] });
    expect(kinds({ ...idle, branch: pr(1, [2, true]) }).parent).toEqual(["saved:d0"]);
    expect(kinds({ ...idle, branch: pr(0, [2, true], [0, false], [1, false]) }).parent).toEqual(["saved:d0", "save:d2"]);
    expect(kinds({ ...idle, branch: pr(1, [0, false]) }).parent).toEqual(["update"]);
    expect(kinds({ ...idle, branch: pr(0, [0, false]) })).toEqual({ own: null, parent: [] });
    expect(kinds({ ...idle, branch: { ...pr(0, [2, true]), pullRequest: { closed: true } } })).toEqual({ own: null, parent: [] });
  });

  it("shows a shutdown after a running attempt and before changes going live, with the PR Environment's rows beside it", () => {
    expect(kinds({ ...idle, shutdown: "off", waiting: true, branch: pr(0, [2, true]) })).toEqual({ own: "shutdown", parent: ["saved:d0"] });
    expect(kinds({ ...idle, shutdown: "running", attempt: "a", branch: pr(0) }).own).toBe("attempt");
    expect(kinds({ ...idle, shutdown: "failed", staged: true, branch: pr(0) }).own).toBe("staged");
  });
});
