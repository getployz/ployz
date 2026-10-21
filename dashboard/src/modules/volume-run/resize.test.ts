import { describe, expect, it } from "vitest";
import { planResize, resizeRequirements, sizeDirection, undoOnRefusal } from "#/modules/volume-run/resize";
import type { AnsweredMember } from "#/modules/volume-run/volume-run";
import { type Copy, member } from "#/modules/volume-run/volume-run.test-fixture";

const GB = 1_000_000_000;

describe("sizeDirection", () => {
  it.each<[string, number, number, unknown]>([
    ["a larger size grows", 10 * GB, 20 * GB, "grow"],
    ["the same size settles", 10 * GB, 10 * GB, "settle"],
    ["a smaller size shrinks", 10 * GB, 5 * GB, "shrink"],
  ])("%s", (_, from, to, expected) => {
    expect(sizeDirection(from, to)).toEqual(expected);
  });
});

describe("planResize", () => {
  const copy = (server: string, kind: Copy, bound?: number): AnsweredMember => {
    const each = member(server, kind, bound === undefined ? {} : { bound });
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

  const settle = (bounds: readonly (number | undefined)[]) => planResize("data", { maximum_bytes: 20 * GB }, { recorded: 20 * GB, working: 20 * GB },
    copy("fsn-1", "writer", bounds[0]), [copy("fsn-2", "mirror", bounds[1]), copy("fsn-3", "mirror", bounds[2])]);

  it("orders a settle as primary, mirrors, no record, when every holder is at or above the size", () => {
    const planned = settle([20 * GB, 30 * GB, 20 * GB]);
    expect(planned).toMatchObject({ ok: true, phase: { kind: "resize", direction: "settle", from: 20 * GB, to: 20 * GB } });
    if (!planned.ok) throw new Error(planned.refusal.message);
    expect(planned.phase.steps.map((step) => (step.kind === "record" ? "record" : `${step.role} ${step.member.machine.name}`)))
      .toEqual(["primary fsn-1", "mirror fsn-2", "mirror fsn-3"]);
  });

  it("refuses a settle that would grow a holder below the recorded size, naming every lowest holder and both commands", () => {
    expect(settle([20 * GB, 10 * GB, 10 * GB])).toEqual({ ok: false, refusal: {
      code: "held_below",
      message: "fsn-2 and fsn-3 hold data at 10 GB, below its recorded 20 GB. Run ployz volume set data --size 10 to record the size it holds, then ployz volume set data --size 20 to grow it",
    } });
    expect(settle([15 * GB, 20 * GB, 20 * GB])).toMatchObject({ ok: false, refusal: { code: "held_below", message: expect.stringMatching(/^fsn-1 holds data at 15 GB, below/) } });
  });

  it("refuses a settle when the frame shows no bound for a holder", () => {
    expect(settle([20 * GB, undefined, 20 * GB])).toEqual({ ok: false, refusal: { code: "unobserved", message: "Cloud has no fresh reading of data on fsn-2; try again in a moment" } });
  });

  it("orders a shrink as record, primary, mirrors", () => {
    const planned = planResize("data", { maximum_bytes: 10 * GB }, { recorded: 20 * GB, working: 20 * GB }, copy("fsn-1", "writer"), [copy("fsn-2", "mirror"), copy("fsn-3", "mirror")]);
    expect(planned).toMatchObject({ ok: true, phase: { kind: "resize", direction: "shrink", from: 20 * GB, to: 10 * GB } });
    if (!planned.ok) throw new Error(planned.refusal.message);
    expect(planned.phase.steps.map((step) => (step.kind === "record" ? "record" : `${step.role} ${step.member.machine.name}`)))
      .toEqual(["record", "primary fsn-1", "mirror fsn-2", "mirror fsn-3"]);
  });

  it.each<[string, number, number, string[]]>([
    ["a grow asks every holder for the size verb", 10 * GB, 20 * GB, ["ployz.volume.size.set.v1"]],
    ["a shrink asks every holder for the size verb and the shrink", 20 * GB, 10 * GB, ["ployz.volume.size.set.v1", "ployz.volume.size.shrink.v1"]],
    ["a settle asks every holder for the size verb and the shrink", 10 * GB, 10 * GB, ["ployz.volume.size.set.v1", "ployz.volume.size.shrink.v1"]],
  ])("%s", (_, from, to, expected) => {
    expect(resizeRequirements(from, to).map((requirement) => requirement.capability)).toEqual(expected);
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
