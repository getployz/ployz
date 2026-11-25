import type { MachineId, MachineName, VolumeCopy } from "@ployz/sdk";
import { expect, it } from "vitest";
import { seedFailureMessage } from "#/modules/volume-run/volume-run.cluster.server";
import { COPIES } from "./volume-run.test-fixture";

const machine = { id: "a".padEnd(32, "0") as MachineId, name: "fsn-1" as MachineName };
const target = COPIES.pulling.pull.target;
const receiving = (failure?: string): VolumeCopy => ({ ...COPIES.pulling, pull_state: { kind: "receiving", target, failure } });

it("ends a seed on a failed copy part of which still runs, and waits on one still receiving", () => {
  const stuck = seedFailureMessage(receiving("cannot receive: out of space"), "data-copy", "data", machine);
  expect(stuck).toContain("cannot receive: out of space");
  expect(stuck).toContain("fsn-1");
  expect(seedFailureMessage(receiving(), "data-copy", "data", machine)).toBeNull();
  expect(seedFailureMessage(COPIES.pulling, "data-copy", "data", machine)).toBeNull();
});
