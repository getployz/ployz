import { describe, expect, it } from "vitest";
import {
  getVariableGroupConfigDiffRows,
  type VariableGroupConfig,
} from "#/modules/environment-design/variable-group-config";

const emptyConfig: VariableGroupConfig = {
  version: 1,
  name: "Shared",
  variables: [],
};

describe("getVariableGroupConfigDiffRows", () => {
  it("stages a new empty Variable Group as a node-level add", () => {
    expect(
      getVariableGroupConfigDiffRows({
        nodeId: "variable-group-1",
        current: emptyConfig,
        baseline: null,
      }),
    ).toEqual([
      {
        changeKey: "variable-group-1:node",
        label: "Variable Group",
        kind: "add",
        path: "node",
        currentValue: "",
        newValue: "Shared",
        canDiscard: true,
      },
    ]);
  });

  it("compares sealed variables by fingerprint, not encrypted payload", () => {
    const baseline: VariableGroupConfig = {
      ...emptyConfig,
      variables: [
        {
          key: "DATABASE_URL",
          description: null,
          exported: true,
          value: {
            type: "sealed",
            hasValue: true,
            fingerprint: "same",
            encryptedValue: {
              version: 1,
              iv: "old-iv",
              tag: "old-tag",
              ciphertext: "old-ciphertext",
            },
          },
        },
      ],
    };
    const current: VariableGroupConfig = {
      ...baseline,
      variables: [
        {
          key: "DATABASE_URL",
          description: null,
          exported: true,
          value: {
            type: "sealed",
            hasValue: true,
            fingerprint: "same",
            encryptedValue: {
              version: 1,
              iv: "new-iv",
              tag: "new-tag",
              ciphertext: "new-ciphertext",
            },
          },
        },
      ],
    };

    expect(
      getVariableGroupConfigDiffRows({
        nodeId: "variable-group-1",
        current,
        baseline,
      }),
    ).toEqual([]);
  });
});
