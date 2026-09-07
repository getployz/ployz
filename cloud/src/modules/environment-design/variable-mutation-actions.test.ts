import { describe, expect, it } from "vitest";
import {
  buildSealServiceVariableUpdateInput,
  type PlainVariableRecord,
} from "#/modules/environment-design/variable-mutation-actions";
import type { VariableRecord } from "#/modules/environment-design/variables";

const baseVariable = {
  id: "33333333-3333-4333-8333-333333333333",
  serviceId: "22222222-2222-4222-8222-222222222222",
  variableGroupId: null,
  configKeyId: "44444444-4444-4444-8444-444444444444",
  key: "API_KEY",
  description: "API token",
  exported: true,
  createdAt: new Date(0),
  updatedAt: new Date(0),
} satisfies Omit<VariableRecord, "value">;

describe("buildSealServiceVariableUpdateInput", () => {
  it("builds a sealed update from the current plain variable value and metadata", () => {
    const variable: PlainVariableRecord = {
      ...baseVariable,
      value: {
        type: "plain",
        value: "secret-token",
      },
    };

    expect(
      buildSealServiceVariableUpdateInput({
        organizationSlug: "acme",
        environmentId: "11111111-1111-4111-8111-111111111111",
        serviceId: "22222222-2222-4222-8222-222222222222",
        variable,
      }),
    ).toEqual({
      organizationSlug: "acme",
      environmentId: "11111111-1111-4111-8111-111111111111",
      serviceId: "22222222-2222-4222-8222-222222222222",
      variableId: "33333333-3333-4333-8333-333333333333",
      key: "API_KEY",
      description: "API token",
      exported: true,
      value: {
        type: "sealed",
        value: "secret-token",
      },
    });
  });

  it("rejects already sealed variables", () => {
    expect(() =>
      buildSealServiceVariableUpdateInput({
        organizationSlug: "acme",
        environmentId: "11111111-1111-4111-8111-111111111111",
        serviceId: "22222222-2222-4222-8222-222222222222",
        variable: {
          ...baseVariable,
          value: {
            type: "sealed",
            hasValue: true,
            fingerprint: "fingerprint",
          },
        },
      }),
    ).toThrow(/only plain variables can be sealed/i);
  });
});
