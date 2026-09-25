import { describe, expect, it } from "vitest";
import { Effect, Schema } from "effect";
import { decodeEnvironmentResourceNodeConfig } from "#/modules/environment-design/environment-resource-node";

describe("Environment Resource node spine", () => {
  it.each([
    ["v1", { version: 1, name: "shared-data" }],
    ["v2 with historical provisioned storage", {
      version: 2,
      name: "shared-data",
      storage: { kind: "provisioned", maxSizeBytes: 10_737_418_240 },
    }],
  ])("normalizes historical %s Volume configs", (_label, input) => {
    expect(Effect.runSync(decodeEnvironmentResourceNodeConfig("volume", input))).toEqual({
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
});
