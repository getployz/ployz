import type { MachineId } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { approvalProgress, EXPIRED_NOTE, FINISHING_NOTE, serverLine } from "./approval-progress";
import type { StepAnswer, VolumeRunView } from "./volume-run";

const a = "a".repeat(32) as MachineId;
const b = "b".repeat(32) as MachineId;
const c = "c".repeat(32) as MachineId;
const EXPIRES = 1_000_000;
const approval = { body: JSON.stringify({ expires_at: EXPIRES, workflow: { kind: "move", from: a, to: b, mirrors: [c] } }), signatures: [] };
const answer = (machine: MachineId, name: string, step: string, accepted: boolean): StepAnswer => ({ machine, name, step, accepted, at: "" });
const run = (state: VolumeRunView["state"], answers: StepAnswer[]) => ({ args: { to: "web-2", approval }, state, answers });
const names = (id: MachineId) => ({ [a]: "web-1", [b]: "web-2", [c]: "web-3" })[id];

describe("approvalProgress", () => {
  it("is nothing for a run that carries no approval", () => {
    expect(approvalProgress({ args: { to: "web-2" }, state: "running", answers: [] }, 0)).toBeNull();
  });

  it("names every Server the approval names, a silent one as not yet answering", () => {
    const progress = approvalProgress(run("running", [answer(a, "web-1", "writer", true), answer(b, "web-2", "writer", false)]), EXPIRES - 1, names);
    expect(progress?.note).toBeNull();
    expect(progress?.servers.map(serverLine)).toEqual(["web-1 took write", "web-2 refused write", "web-3 hasn't answered"]);
  });

  it.each<[string, VolumeRunView["state"], StepAnswer[], string | null]>([
    ["a run past its approval that a Server already took finishes it", "running", [answer(a, "web-1", "writer", true)], FINISHING_NOTE],
    ["a run past its approval that no Server took starts nothing", "running", [], EXPIRED_NOTE],
    ["a run past its approval that no Server took, still waiting to start, starts nothing", "requested", [answer(b, "web-2", "writer", false)], EXPIRED_NOTE],
    ["a run that ended on its expired approval says so", "failed", [answer(a, "web-1", "writer", true)], EXPIRED_NOTE],
    ["a run that finished needs no note", "done", [answer(a, "web-1", "writer", true)], null],
  ])("%s", (_, state, answers, note) => {
    expect(approvalProgress(run(state, answers), EXPIRES, names)?.note).toBe(note);
  });

  it("lets a Server's latest answer to a step stand", () => {
    const progress = approvalProgress(run("running", [answer(b, "web-2", "mirror", false), answer(b, "web-2", "mirror", true), answer(b, "web-2", "writer", true)]), 0, names);
    expect(progress?.servers.find((server) => server.machine === b)?.steps).toEqual([{ step: "mirror", accepted: true }, { step: "writer", accepted: true }]);
    expect(serverLine({ machine: b, name: "web-2", steps: [{ step: "mirror", accepted: true }, { step: "none", accepted: false }] })).toBe("web-2 took mirror, refused delete its copy");
  });
});
