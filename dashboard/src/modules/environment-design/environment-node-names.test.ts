import { describe, expect, it } from "vitest";
import { Schema } from "effect";
import {
  createEnvironmentNodeNameSchema,
  getDuplicateEnvironmentNodeNameMessage,
  isEnvironmentNodeNameTaken,
  normalizeEnvironmentNodeName,
  resolveUniqueEnvironmentNodeName,
} from "#/modules/environment-design/environment-node-names";
import { decodeStrict } from "#/modules/environment-design/schema";

const nameSchema = Schema.Trim.check(
  Schema.isNonEmpty(),
  Schema.isMaxLength(64),
);

describe("environment node names", () => {
  it("normalizes names by trimming and comparing case-insensitively", () => {
    expect(normalizeEnvironmentNodeName(" API ")).toBe("api");
    expect(
      isEnvironmentNodeNameTaken("API", [
        {
          type: "service",
          id: "service-1",
          name: "api",
        },
      ]),
    ).toBe(true);
  });

  it("detects duplicate names across node types while excluding the current node", () => {
    expect(
      isEnvironmentNodeNameTaken(
        "database",
        [
          {
            type: "variable_group",
            id: "resource-1",
            name: "Database",
          },
        ],
        {
          type: "service",
          id: "service-1",
        },
      ),
    ).toBe(true);

    expect(
      isEnvironmentNodeNameTaken(
        "DATABASE",
        [
          {
            type: "variable_group",
            id: "resource-1",
            name: "Database",
          },
        ],
        {
          type: "variable_group",
          id: "resource-1",
        },
      ),
    ).toBe(false);
  });

  it("uses environment-node copy for duplicate messages", () => {
    expect(getDuplicateEnvironmentNodeNameMessage("database")).toBe(
      'A node named "database" already exists in this environment.',
    );
  });

  it("builds a schema refinement for client-side duplicate validation", () => {
    const schema = createEnvironmentNodeNameSchema({
      schema: nameSchema,
      nodes: [
        {
          type: "service",
          id: "service-1",
          name: "api",
        },
      ],
      excludeNode: {
        type: "variable_group",
        id: "resource-1",
      },
    });

    expect(() => decodeStrict(schema, "API")).toThrow(
      'A node named "API" already exists in this environment.',
    );
  });

  it("adds a short suffix for generated duplicate names", () => {
    const result = resolveUniqueEnvironmentNodeName({
      name: "api",
      schema: nameSchema,
      nodes: [
        {
          type: "service",
          id: "service-1",
          name: "API",
        },
      ],
      randomSuffix: () => "a1b2",
    });

    expect(result).toBe("api-a1b2");
  });

  it("keeps suffixed generated names within the name length limit", () => {
    const result = resolveUniqueEnvironmentNodeName({
      name: "a".repeat(64),
      schema: nameSchema,
      maxLength: 64,
      nodes: [
        {
          type: "variable_group",
          id: "resource-1",
          name: "a".repeat(64),
        },
      ],
      randomSuffix: () => "zzzz",
    });

    expect(result).toHaveLength(64);
    expect(result.endsWith("-zzzz")).toBe(true);
  });
});
