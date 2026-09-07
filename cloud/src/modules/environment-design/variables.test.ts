import { decodeStrict } from "#/modules/environment-design/schema";
import { describe, expect, it } from "vitest";
import {
  createServiceVariableSchema,
  updateServiceVariableSchema,
  variableValuePartsSchema,
} from "#/modules/environment-design/variables";

const baseInput = {
  organizationSlug: "acme",
  environmentId: "11111111-1111-4111-8111-111111111111",
  serviceId: "22222222-2222-4222-8222-222222222222",
  description: null,
  exported: false,
  value: {
    type: "plain" as const,
    value: "80",
  },
};

describe("variable schemas", () => {
  it("normalizes created variable keys to uppercase", () => {
    expect(
      decodeStrict(createServiceVariableSchema, {
        ...baseInput,
        key: "port",
      }).key,
    ).toBe("PORT");
  });

  it("normalizes updated variable keys to uppercase", () => {
    expect(
      decodeStrict(updateServiceVariableSchema, {
        ...baseInput,
        variableId: "33333333-3333-4333-8333-333333333333",
        key: "Api_Key",
      }).key,
    ).toBe("API_KEY");
  });

  it("accepts sealed value inputs for variable updates", () => {
    const parsed = decodeStrict(updateServiceVariableSchema, {
      ...baseInput,
      variableId: "33333333-3333-4333-8333-333333333333",
      key: "API_KEY",
      value: {
        type: "sealed",
        value: "secret-token",
      },
    });

    expect(parsed.value).toEqual({
      type: "sealed",
      value: "secret-token",
    });
  });

  it("rejects empty sealed value inputs for variable updates", () => {
    expect(() =>
      decodeStrict(updateServiceVariableSchema, {
        ...baseInput,
        variableId: "33333333-3333-4333-8333-333333333333",
        key: "API_KEY",
        value: {
          type: "sealed",
          value: "",
        },
      }),
    ).toThrow(/sealed value is required/i);
  });

  it("strictly decodes persisted structured value parts", () => {
    expect(() =>
      decodeStrict(variableValuePartsSchema, [
        {
          kind: "ref",
          owner: { scope: "self" },
          key: "PORT",
          plaintext: "must not cross the persisted boundary",
        },
      ]),
    ).toThrow(/plaintext/u);
  });
});
