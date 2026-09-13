import { describe, expect, it } from "vitest";
import { environmentNodeIntroductionSchema } from "#/modules/environment-design/environment-node-introductions";
import { decodeStrict } from "#/modules/environment-design/schema";

const base = {
  organizationId: crypto.randomUUID(),
  environmentId: crypto.randomUUID(),
  nodeId: crypto.randomUUID(),
  nodeLineageId: crypto.randomUUID(),
  configVersion: 1 as const,
  createdAt: new Date(),
  updatedAt: new Date(),
};

describe("environment node introductions", () => {
  it("strictly parses every supported node config", () => {
    expect(
      decodeStrict(environmentNodeIntroductionSchema, {
        ...base,
        nodeType: "service",
        config: {
          version: 2,
          name: "api",
          source: { version: 1, type: "image", image: "nginx", autoUpdate: { type: "off" }, credentials: { type: "none" } },
          preDeployCommand: null,
          startCommand: null,
          healthcheck: { type: "none" },
          restartPolicy: "unless-stopped",
          privateDns: "api",
        },
      }).nodeType,
    ).toBe("service");

    expect(
      decodeStrict(environmentNodeIntroductionSchema, {
        ...base,
        nodeType: "variable_group",
        config: { version: 1, name: "Shared", variables: [] },
      }).nodeType,
    ).toBe("variable_group");

    expect(
      decodeStrict(environmentNodeIntroductionSchema, {
        ...base,
        nodeType: "volume",
        configVersion: 2,
        config: { version: 2, name: "Data" },
      }).nodeType,
    ).toBe("volume");
  });
});
