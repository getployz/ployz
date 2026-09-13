// @vitest-environment jsdom

import { describe, expect, it } from "vitest";
import {
  getValueAtPath,
  setDraftValueAtPath,
} from "#/utils/schema-path";

describe("schema-path helpers", () => {
  it("gets nested values and mutates drafts by path", () => {
    const value = {
      metadata: {
        title: "API",
      },
    };

    expect(getValueAtPath(value, "metadata.title")).toBe("API");

    setDraftValueAtPath(value, "metadata.title", "Worker");

    expect(value).toEqual({
      metadata: {
        title: "Worker",
      },
    });
  });
});
