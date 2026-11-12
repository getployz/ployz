import { describe, expect, it } from "vitest";
import { runChange } from "./run-approval";
import { member } from "./volume-run.test-fixture";

const GB = 1_000_000_000;
const members = [member("fsn-1", "writer"), member("fsn-2", "mirror"), member("fsn-3", "empty")];
const terms = { running: null, recorded: 10 * GB };
const resize = (gigabytes: number) => runChange("data", { kind: "resize", args: { maximum_bytes: gigabytes * GB } }, members, terms);

describe("runChange for a resize", () => {
  it("names every Server holding a copy, and approves a resize to the recorded size or below as a shrink, as the CLI signs it", () => {
    expect([resize(20), resize(10), resize(5)].map(({ title, action, workflow }) => ({ title, action, shrink: workflow.kind === "resize" && workflow.shrink })))
      .toEqual([
        { title: "Grow data", action: "Approve grow", shrink: false },
        { title: "Resize data", action: "Approve resize", shrink: true },
        { title: "Shrink data", action: "Approve shrink", shrink: true },
      ]);
    expect(resize(20).parties).toEqual([members[0]?.machine.id, members[1]?.machine.id]);
  });
});
