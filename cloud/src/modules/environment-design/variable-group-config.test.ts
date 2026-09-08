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

});
