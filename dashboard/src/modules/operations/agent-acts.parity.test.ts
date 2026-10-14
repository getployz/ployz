import { readFileSync } from "node:fs";
import { Schema } from "effect";
import { describe, expect, it } from "vitest";
import { ACTS, STORE_WRITES } from "../../../evals/agent/acted";

const AGENT_TOOLS = new URL("../../../../core/crates/ployz-store/tests/snapshots/agent_tools.json", import.meta.url);

const AgentTools = Schema.fromJsonString(Schema.Struct({
  tools: Schema.Array(Schema.Struct({ name: Schema.String, read_only: Schema.Boolean })),
}));
const { tools } = Schema.decodeUnknownSync(AgentTools)(readFileSync(AGENT_TOOLS, "utf8"));
const writers = tools.filter((tool) => !tool.read_only).map((tool) => tool.name);

describe("agent eval acts", () => {
  it("counts every agent tool that writes or acts as acting, so a read-only task fails when the agent calls one", () => {
    expect(writers.filter((tool) => !ACTS.has(tool) && !STORE_WRITES.has(tool))).toEqual([]);
  });

  it("reads the real snapshot, so an empty tool list cannot pass", () => {
    expect(writers).toContain("server_forget");
  });
});
