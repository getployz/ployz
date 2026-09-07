import { describe, expect, it } from "vitest";
import type { MachineId } from "@ployz/sdk";
import {
  confirmedVolumeRemove,
  directVolumeDataLoss,
  unionDataLossLists,
  withMissingDataLossIdentities,
  type CloudRowLoss,
  type DataLossList,
} from "./data-loss-confirm";
import {
  dataLossIdentityKey,
  dataLossIdentityLabel,
  type DataLossIdentity,
} from "./data-loss-identity";

const machineA = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" as MachineId;
const machineB = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" as MachineId;

function rustVolume(
  machine_id: MachineId,
  name: string,
): DataLossIdentity {
  return { kind: "docker_volume", id: { machine_id, name } };
}

function cloudRow(kind: string, name: string): CloudRowLoss {
  return { kind, name };
}

function list(overrides: Partial<DataLossList> = {}): DataLossList {
  return { rust: [], cloud: [], ...overrides };
}

describe("Data Loss identities", () => {
  it("treats the same volume name on two machines as distinct identities", () => {
    const left = rustVolume(machineA, "data");
    const right = rustVolume(machineB, "data");

    expect(dataLossIdentityKey(left)).not.toBe(dataLossIdentityKey(right));
    expect(dataLossIdentityLabel(left)).toBe(`data on ${machineA}`);
    expect(dataLossIdentityLabel(right)).toBe(`data on ${machineB}`);
    expect(dataLossIdentityLabel(left)).not.toBe("data");
  });

  it("unions rust identities and Cloud row-loss from several lists", () => {
    const env1 = list({
      rust: [rustVolume(machineA, "pg-data")],
      cloud: [cloudRow("environment", "acme/web/production")],
    });
    const env2 = list({
      rust: [rustVolume(machineB, "pg-data")],
      cloud: [cloudRow("environment", "acme/web/staging")],
    });
    const env3 = list({
      rust: [rustVolume(machineA, "pg-data")],
      cloud: [cloudRow("environment", "acme/web/preview")],
    });
    const env4 = list({
      rust: [rustVolume(machineA, "uploads")],
      cloud: [cloudRow("environment", "acme/web/dev")],
    });
    const env5 = list({
      rust: [],
      cloud: [cloudRow("project", "acme/web")],
    });

    const united = unionDataLossLists([env1, env2, env3, env4, env5]);

    expect(united.rust).toEqual([
      rustVolume(machineA, "pg-data"),
      rustVolume(machineB, "pg-data"),
      rustVolume(machineA, "uploads"),
    ]);
    expect(united.cloud).toEqual([
      cloudRow("environment", "acme/web/production"),
      cloudRow("environment", "acme/web/staging"),
      cloudRow("environment", "acme/web/preview"),
      cloudRow("environment", "acme/web/dev"),
      cloudRow("project", "acme/web"),
    ]);
    expect(confirmedVolumeRemove(united)).toEqual([
      { machine_id: machineA, name: "pg-data" },
      { machine_id: machineB, name: "pg-data" },
      { machine_id: machineA, name: "uploads" },
    ]);
  });

  it("keeps Cloud row-loss out of the rust confirm list Inngest will send", () => {
    const shown = list({
      rust: [rustVolume(machineA, "pg-data")],
      cloud: [
        cloudRow("environment", "acme/web/production"),
        cloudRow("project", "acme/web"),
      ],
    });

    expect(confirmedVolumeRemove(shown)).toEqual([
      { machine_id: machineA, name: "pg-data" },
    ]);
  });

  it("builds DIRECT volume confirmation from SDK volume ids", () => {
    const shown = directVolumeDataLoss([
      { machine_id: machineA, name: "data" },
      { machine_id: machineB, name: "data" },
    ]);

    expect(shown.rust).toEqual([
      rustVolume(machineA, "data"),
      rustVolume(machineB, "data"),
    ]);
    expect(shown.cloud).toEqual([]);
    expect(confirmedVolumeRemove(shown)).toEqual([
      { machine_id: machineA, name: "data" },
      { machine_id: machineB, name: "data" },
    ]);
  });

  it("re-opens CASCADE with missing identities without dropping vanished ones", () => {
    const shown = list({
      rust: [rustVolume(machineA, "old")],
      cloud: [cloudRow("machine", machineA)],
    });
    const missing = [rustVolume(machineA, "new")];

    expect(withMissingDataLossIdentities(shown, missing)).toEqual(
      list({
        rust: [rustVolume(machineA, "old"), rustVolume(machineA, "new")],
        cloud: [cloudRow("machine", machineA)],
      }),
    );
  });
});
