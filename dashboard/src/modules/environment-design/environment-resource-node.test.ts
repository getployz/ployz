import { describe, expect, it } from "vitest";
import { Effect, Schema } from "effect";
import {
  decodeEnvironmentResourceNodeConfig,
  getEnvironmentResourceNodeConfigDiffRows,
  getEnvironmentResourceNodeSnapshotResourceName,
  isEnvironmentResourceNodeType,
} from "#/modules/environment-design/environment-resource-node";
import { namedVolumeConfig } from "#/modules/environment-design/volume-config";

const nodeId = "11111111-1111-4111-8111-111111111111";

describe("Environment Resource node spine", () => {
  it("owns Environment Resource type recognition and snapshot names", () => {
    expect(isEnvironmentResourceNodeType("volume")).toBe(true);
    expect(isEnvironmentResourceNodeType("service")).toBe(false);
    expect(getEnvironmentResourceNodeSnapshotResourceName("volume")).toBe(
      "VolumeSnapshot",
    );
  });

  it("normalizes historical Volume configs", () => {
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
    const failure = Effect.runSync(
      Effect.flip(
        decodeEnvironmentResourceNodeConfig("volume", {
          version: 2,
          name: "shared-data",
          extra: true,
        }),
      ),
    );
    expect(Schema.isSchemaError(failure)).toBe(true);
  });

  it("diffs explicit resource projections without selecting a baseline", () => {
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
