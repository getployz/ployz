import { describe, expect, it } from "vitest";
import { directionOf, planResize, RESIZE_ORDER, undoOnRefusal } from "#/modules/volume-run/resize";
import type { AnsweredMember } from "#/modules/volume-run/volume-run";
import { type Copy, member } from "#/modules/volume-run/volume-run.test-fixture";

const GB = 1_000_000_000;

describe("directionOf", () => {
  it.each<[string, number, number, unknown]>([
    ["a larger size grows", 10 * GB, 20 * GB, "grow"],
    ["the same size is settled", 10 * GB, 10 * GB, { code: "already_sized", message: "data already has 10 GB" }],
    ["a smaller size is refused", 10 * GB, 5 * GB, { code: "invalid", message: "data has 10 GB; shrinking a deployed Volume isn't supported yet" }],
    ["a fraction prints exactly", 1_001_000_000, 1_001_000_000, { code: "already_sized", message: "data already has 1.001 GB" }],
  ])("%s", (_, from, to, expected) => {
    expect(directionOf("data", from, to)).toEqual(expected);
  });
});

describe("planResize", () => {
  const copy = (server: string, kind: Copy): AnsweredMember => {
    const each = member(server, kind);
    if (!each.answered) throw new Error(`${server} did not answer`);
    return each;
  };

  it("orders a grow as mirrors, primary, record", () => {
    const planned = planResize("data", { maximum_bytes: 20 * GB }, { recorded: 10 * GB, working: 10 * GB }, copy("fsn-1", "writer"), [copy("fsn-2", "mirror"), copy("fsn-3", "mirror")]);
    expect(planned).toMatchObject({ ok: true, phase: { kind: "resize", direction: "grow", from: 10 * GB, to: 20 * GB } });
    if (!planned.ok) throw new Error(planned.refusal.message);
    expect(planned.phase.steps.map((step) => (step.kind === "record" ? "record" : `${step.role} ${step.member.machine.name}`)))
      .toEqual(["mirror fsn-2", "mirror fsn-3", "primary fsn-1", "record"]);
  });

  it("asks every holder for the resize verb", () => {
    expect(RESIZE_ORDER.grow.requires.map((requirement) => requirement.capability)).toEqual(["ployz.volume.size.set.v1"]);
  });
});

describe("undoOnRefusal", () => {
  it.each<[string, ("recorded" | "held")[], boolean]>([
    ["nothing done", [], false],
    ["a record and no holder", ["recorded"], true],
    ["a record then a holder", ["recorded", "held"], false],
    ["holders then a record, as a grow does", ["held", "held", "recorded"], false],
    ["holders and no record", ["held"], false],
  ])("after %s", (_, done, expected) => {
    expect(undoOnRefusal(done)).toBe(expected);
  });
});
