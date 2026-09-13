import { describe, expect, it } from "vitest";
import { Effect, Exit } from "effect";
import {
  decodeEnvironmentResourceNodeConfig,
  getEnvironmentResourceNodeConfigDiffRows,
  getEnvironmentResourceNodeSnapshotResourceName,
  isEnvironmentResourceNodeType,
  parseEnvironmentResourceNodeConfig,
} from "#/modules/environment-design/environment-resource-node";
import { namedVolumeConfig } from "#/modules/environment-design/volume-config";

const nodeId = "11111111-1111-4111-8111-111111111111";

describe("Environment Resource node spine", () => {
  it("owns Environment Resource type recognition and snapshot names", () => {
    expect(isEnvironmentResourceNodeType("variable_group")).toBe(true);
    expect(isEnvironmentResourceNodeType("volume")).toBe(true);
    expect(isEnvironmentResourceNodeType("service")).toBe(false);
    expect(getEnvironmentResourceNodeSnapshotResourceName("variable_group")).toBe(
      "VariableGroupSnapshot",
    );
    expect(getEnvironmentResourceNodeSnapshotResourceName("volume")).toBe(
      "VolumeSnapshot",
    );
  });

  it("strictly parses Variable Groups and normalizes historical Volume configs", () => {
    expect(
      parseEnvironmentResourceNodeConfig("volume", {
        version: 1,
        name: "shared-data",
      }),
    ).toEqual({
      version: 2,
      name: "shared-data",
    });

    expect(
      parseEnvironmentResourceNodeConfig("variable_group", {
        version: 1,
        name: "Shared",
        variables: [],
      }),
    ).toEqual({ version: 1, name: "Shared", variables: [] });

    expect(
      Effect.runSync(
        decodeEnvironmentResourceNodeConfig("volume", {
          version: 1,
          name: "shared-data",
        }),
      ),
    ).toEqual({
      nodeType: "volume",
      config: { version: 2, name: "shared-data" },
    });
  });

  it("rejects extra config fields", () => {
    expect(() =>
      parseEnvironmentResourceNodeConfig("variable_group", {
        version: 1,
        name: "Shared",
        variables: [],
        extra: true,
      }),
    ).toThrow();

    expect(
      Exit.isFailure(
        Effect.runSyncExit(
          decodeEnvironmentResourceNodeConfig("variable_group", {
            version: 1,
            name: "Shared",
            variables: [],
            extra: true,
          }),
        ),
      ),
    ).toBe(true);
  });

  it("diffs explicit resource projections without selecting a baseline", () => {
    expect(
      getEnvironmentResourceNodeConfigDiffRows({
        nodeType: "variable_group",
        nodeId,
        baseline: { version: 1, name: "Shared", variables: [] },
        current: { version: 1, name: "Shared Next", variables: [] },
      }),
    ).toEqual([
      expect.objectContaining({
        path: "name",
        currentValue: "Shared",
        newValue: "Shared Next",
      }),
    ]);

    expect(
      getEnvironmentResourceNodeConfigDiffRows({
        nodeType: "volume",
        nodeId,
        baseline: namedVolumeConfig("data"),
        current: namedVolumeConfig("data-next"),
      }),
    ).toEqual([
      expect.objectContaining({
        path: "name",
        currentValue: "data",
        newValue: "data-next",
      }),
    ]);
  });
});
