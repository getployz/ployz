import { describe, expect, it } from "vitest";
import {
  getDeletedDeployedVolumeResourceIds,
  getVolumeConfigDiffRows,
  getVolumePhysicalName,
  namedVolumeConfig,
  parseVolumeConfig,
} from "#/modules/environment-design/volume-config";

const config = namedVolumeConfig;

describe("getVolumeConfigDiffRows", () => {
  it("stages an unsnapshotted volume as an add", () => {
    const rows = getVolumeConfigDiffRows({
      nodeId: "vol-1",
      current: config("shared-data"),
      baseline: null,
    });

    expect(rows).toEqual([
      {
        changeKey: "vol-1:node",
        label: "Volume",
        kind: "add",
        path: "node",
        currentValue: "",
        newValue: "shared-data",
        canDiscard: true,
      },
    ]);
  });

  it("stages a rename as a name update", () => {
    const rows = getVolumeConfigDiffRows({
      nodeId: "vol-1",
      current: config("renamed"),
      baseline: config("shared-data"),
    });

    expect(rows).toEqual([
      {
        changeKey: "vol-1:name",
        label: "Name",
        kind: "update",
        path: "name",
        currentValue: "shared-data",
        newValue: "renamed",
        canDiscard: false,
      },
    ]);
  });

  it("emits no rows when nothing changed", () => {
    expect(
      getVolumeConfigDiffRows({
        nodeId: "vol-1",
        current: config("shared-data"),
        baseline: config("shared-data"),
      }),
    ).toEqual([]);
  });
});

describe("getVolumePhysicalName", () => {
  it("derives a stable name from the resource id (not the display name)", () => {
    expect(getVolumePhysicalName("abc")).toBe("vol-abc");
  });
});

describe("named Docker volumes", () => {
  it("reads historical v1 snapshots as named volumes", () => {
    expect(parseVolumeConfig({ version: 1, name: "data" })).toEqual({
      version: 2,
      name: "data",
    });
  });

  it("strips historical provisioned storage from v2 snapshots", () => {
    expect(
      parseVolumeConfig({
        version: 2,
        name: "data",
        storage: { kind: "provisioned", maxSizeBytes: 10_737_418_240 },
      }),
    ).toEqual({
      version: 2,
      name: "data",
    });
  });

  it("rejects unknown v2 snapshot fields other than historical storage", () => {
    expect(() =>
      parseVolumeConfig({
        version: 2,
        name: "data",
        extra: true,
      }),
    ).toThrow();
  });
});

describe("getDeletedDeployedVolumeResourceIds", () => {
  it("returns applied identities that are absent from the desired set", () => {
    expect(
      getDeletedDeployedVolumeResourceIds({
        desiredVolumeResourceIds: ["keep"],
        appliedVolumeResourceIds: ["keep", "gone"],
      }),
    ).toEqual(["gone"]);
  });

  it("returns nothing when a never-deployed volume is deleted", () => {
    expect(
      getDeletedDeployedVolumeResourceIds({
        desiredVolumeResourceIds: [],
        appliedVolumeResourceIds: [],
      }),
    ).toEqual([]);
  });
});
