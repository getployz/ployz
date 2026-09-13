import { describe, expect, it } from "vitest";
import {
  findConsumersOfKey,
  findConsumersOfOwner,
} from "#/modules/environment-design/variable-references";
import type { VariableRecord } from "#/modules/environment-design/variables";

function plain(id: string, key: string, value: string): VariableRecord {
  return {
    id,
    serviceId: "svc",
    variableGroupId: null,

    key,
    description: null,
    exported: false,
    value: { type: "plain", value },
    createdAt: new Date(0),
    updatedAt: new Date(0),
  };
}

describe("findConsumersOfOwner", () => {
  const variables: VariableRecord[] = [
    plain("1", "DATABASE_URL", "postgres://u:${{ db.PASSWORD }}@${{ db.PLOYZ_PRIVATE_DOMAIN }}/app"),
    plain("2", "QUEUE_URL", "redis://${{ redis.HOST }}"),
    plain("3", "REGION", "us-east"),
    plain("4", "SELF", "${{ LOCAL }}"),
  ];

  it("finds variables referencing any key of an owner", () => {
    const consumers = findConsumersOfOwner(variables, "db");
    expect(consumers).toEqual([
      { variableId: "1", variableKey: "DATABASE_URL", key: "PASSWORD" },
      { variableId: "1", variableKey: "DATABASE_URL", key: "PLOYZ_PRIVATE_DOMAIN" },
    ]);
  });

  it("ignores literals and self refs", () => {
    expect(findConsumersOfOwner(variables, "redis")).toEqual([
      { variableId: "2", variableKey: "QUEUE_URL", key: "HOST" },
    ]);
    // No owner slug "us-east"; self ref has no owner slug.
    expect(findConsumersOfOwner(variables, "us-east")).toEqual([]);
  });

  it("narrows to a specific key for rename impact", () => {
    expect(findConsumersOfKey(variables, "db", "PASSWORD")).toEqual([
      { variableId: "1", variableKey: "DATABASE_URL", key: "PASSWORD" },
    ]);
    expect(findConsumersOfKey(variables, "db", "MISSING")).toEqual([]);
  });
});
