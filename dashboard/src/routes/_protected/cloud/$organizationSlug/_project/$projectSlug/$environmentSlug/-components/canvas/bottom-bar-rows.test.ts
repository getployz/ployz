import { describe, expect, it } from "vitest";
import { bottomBarRows, type BottomBarState } from "./bottom-bar-rows";

const idle: BottomBarState = { startingPoint: false, staged: false, attempt: false, waiting: false, branch: null };
const branch = (changes: number, updates: number, pullRequest = false) => ({ pullRequest, changes, updates });

describe("bottomBarRows", () => {
  it("gives a root one row: the first of changes to deploy, a running attempt, changes waiting for a pull request", () => {
    expect(bottomBarRows(idle)).toEqual({ own: null, parent: null });
    expect(bottomBarRows({ ...idle, staged: true, attempt: true, waiting: true })).toEqual({ own: "staged", parent: null });
    expect(bottomBarRows({ ...idle, attempt: true, waiting: true })).toEqual({ own: "attempt", parent: null });
    expect(bottomBarRows({ ...idle, waiting: true })).toEqual({ own: "waiting", parent: null });
  });

  it("puts a starting point before its staged nodes", () => {
    expect(bottomBarRows({ ...idle, startingPoint: true, staged: true, branch: branch(0, 0) })).toEqual({ own: "starting_point", parent: null });
  });

  it("gives a Branch a second row for Save in every state of the first", () => {
    for (const own of [
      { startingPoint: true, staged: true }, { staged: true }, { attempt: true }, { staged: true, attempt: true }, {},
    ]) {
      expect(bottomBarRows({ ...idle, ...own, branch: branch(3, 1) }).parent).toBe("save");
    }
    expect(bottomBarRows({ ...idle, staged: true, branch: branch(3, 0) })).toEqual({ own: "staged", parent: "save" });
    expect(bottomBarRows({ ...idle, branch: branch(3, 0) })).toEqual({ own: null, parent: "save" });
  });

  it("shows updates from the Parent once nothing is left to save, and no second row when there's neither", () => {
    expect(bottomBarRows({ ...idle, attempt: true, branch: branch(0, 1) })).toEqual({ own: "attempt", parent: "update" });
    expect(bottomBarRows({ ...idle, staged: true, branch: branch(0, 0) })).toEqual({ own: "staged", parent: null });
  });

  it("keeps a PR Environment's own second row", () => {
    expect(bottomBarRows({ ...idle, staged: true, branch: branch(2, 0, true) })).toEqual({ own: "staged", parent: "pull_request" });
    expect(bottomBarRows({ ...idle, branch: branch(0, 1, true) })).toEqual({ own: null, parent: "pull_request" });
    expect(bottomBarRows({ ...idle, branch: branch(0, 0, true) })).toEqual({ own: null, parent: null });
  });
});
