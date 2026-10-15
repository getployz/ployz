import { describe, expect, it } from "vitest";
import { storeWriteInput } from "#/modules/config-store/store.contract";
import { strictValidator } from "#/server/tanstack";

const here = { project: "web", environment: "production" };
const write = (shows: unknown[]) => strictValidator(storeWriteInput)["~standard"].validate({
  organizationSlug: "acme",
  command: { command: "create_volume", environment: here, id: "vol-1", name: "data" },
  shows,
});

describe("storeWriteInput", () => {
  it("accepts shown views whose Environments are well formed", () => {
    expect(write([
      { query: "volumes", environment: here },
      { query: "sync", from: here, into: { project: "web", environment: "staging" } },
      { query: "sync", from: here },
      { query: "projects" },
    ])).toHaveProperty("value");
  });

  it.each([
    ["a Sync without its source", { query: "sync" }],
    ["a Sync whose target is not an Environment", { query: "sync", from: here, into: "staging" }],
    ["a view whose Environment is not one", { query: "volumes", environment: "production" }],
    ["an entry naming no query", { environment: here }],
  ])("refuses %s before the write runs", (_, shown) => {
    expect(write([shown])).toHaveProperty("issues");
  });
});
