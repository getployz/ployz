import { describe, expect, it } from "vitest";
import { bottomBarRows, type BottomBarState } from "./bottom-bar-rows";

const idle: BottomBarState = { startingPoint: false, staged: false, attempt: false, off: false, waiting: false, branch: null };
const branch = (changes: number, updates: number) => ({ changes, updates, destinations: null });
const pr = (updates: number, ...destinations: Array<[changes: number, saved: boolean]>) => ({
  changes: 0, updates, destinations: destinations.map(([changes, saved]) => ({ changes, saved })),
});
const save = { row: "save", destination: null };

describe("bottomBarRows", () => {
  it("gives a root one row: the first of changes to deploy, a running attempt, changes going live with a pull request", () => {
    expect(bottomBarRows(idle)).toEqual({ own: null, parent: [] });
    expect(bottomBarRows({ ...idle, staged: true, attempt: true, waiting: true })).toEqual({ own: "staged", parent: [] });
    expect(bottomBarRows({ ...idle, attempt: true, waiting: true })).toEqual({ own: "attempt", parent: [] });
    expect(bottomBarRows({ ...idle, waiting: true })).toEqual({ own: "waiting", parent: [] });
  });

  it("puts a starting point before its staged nodes", () => {
    expect(bottomBarRows({ ...idle, startingPoint: true, staged: true, branch: branch(0, 0) })).toEqual({ own: "starting_point", parent: [] });
  });

  it("gives a Branch a second row for Save in every state of the first", () => {
    for (const own of [
      { startingPoint: true, staged: true }, { staged: true }, { attempt: true }, { staged: true, attempt: true }, {},
    ]) {
      expect(bottomBarRows({ ...idle, ...own, branch: branch(3, 1) }).parent).toEqual([save]);
    }
    expect(bottomBarRows({ ...idle, staged: true, branch: branch(3, 0) })).toEqual({ own: "staged", parent: [save] });
    expect(bottomBarRows({ ...idle, branch: branch(3, 0) })).toEqual({ own: null, parent: [save] });
  });

  it("shows updates from the Parent once nothing is left to save, and no second row when there's neither", () => {
    expect(bottomBarRows({ ...idle, attempt: true, branch: branch(0, 1) })).toEqual({ own: "attempt", parent: [{ row: "update", destination: null }] });
    expect(bottomBarRows({ ...idle, staged: true, branch: branch(0, 0) })).toEqual({ own: "staged", parent: [] });
  });

  it("gives a PR Environment a row per Destination with changes: to save, or saved to go live with the pull request", () => {
    expect(bottomBarRows({ ...idle, staged: true, branch: pr(0, [2, false]) })).toEqual({ own: "staged", parent: [{ row: "save", destination: 0 }] });
    expect(bottomBarRows({ ...idle, branch: pr(1, [2, true]) }).parent).toEqual([{ row: "saved", destination: 0 }]);
    expect(bottomBarRows({ ...idle, branch: pr(0, [2, true], [0, false], [1, false]) }).parent)
      .toEqual([{ row: "saved", destination: 0 }, { row: "save", destination: 2 }]);
    expect(bottomBarRows({ ...idle, branch: pr(1, [0, false]) }).parent).toEqual([{ row: "update", destination: null }]);
    expect(bottomBarRows({ ...idle, branch: pr(0, [0, false]) })).toEqual({ own: null, parent: [] });
  });

  it("shows Off after a running attempt and before changes going live, with the PR Environment's rows beside it", () => {
    expect(bottomBarRows({ ...idle, off: true, waiting: true, branch: pr(0, [2, true]) })).toEqual({ own: "off", parent: [{ row: "saved", destination: 0 }] });
    expect(bottomBarRows({ ...idle, off: true, attempt: true, branch: pr(0) }).own).toBe("attempt");
    expect(bottomBarRows({ ...idle, off: true, staged: true, branch: pr(0) }).own).toBe("staged");
  });
});
