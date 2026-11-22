import { readFileSync } from "node:fs";
import { type Answer, type Write, agentStart } from "@ployz/sdk";
import { Effect } from "effect";
import { describe, expect, it } from "vitest";
import { AGENT_TOOLS, ASKS, INTENT_DESCRIPTION, INTENT_REFUSAL } from "./agent-runner.server";
import { GATED, finish, untilWrite } from "./agent-tools";

const place = { project: "shop", environment: "production", store: { at: "cloud", organization: "org" } } as const;
const empty = { view: "services", services: [] };

describe("agent tools", () => {
  it("gates a write that may ask a human, and not a save or a read", () => {
    expect(GATED).toContain("deploy");
    expect(GATED).toContain("server_remove");
    expect(GATED).not.toContain("publish");
    expect(GATED).not.toContain("environment_show");
  });

  it("ends the description of every tool that may ask with the same sentence, and of no other", () => {
    const asks = AGENT_TOOLS.filter((tool) => tool.description.includes(ASKS)).map((tool) => tool.name);
    expect(asks.sort()).toEqual([...GATED].sort());
  });

  it("words the sentences it shares with `ployz mcp` as the Rust source does", () => {
    const rust = (path: string) => readFileSync(new URL(`../../../../core/crates/${path}`, import.meta.url), "utf8");
    const mcp = rust("ployz/src/agent_call/mcp.rs");
    expect(mcp).toContain(`const INTENT_DESCRIPTION: &str = ${JSON.stringify(INTENT_DESCRIPTION)};`);
    expect(mcp).toContain(`const INTENT_REQUIRED: &str = ${JSON.stringify(INTENT_REFUSAL)};`);
    expect(rust("ployz-agent-tools/src/step.rs")).toContain(JSON.stringify(ASKS));
  });

  it("marks destructive the tools that delete something when they run, and never a read", () => {
    const destructive = new Set(AGENT_TOOLS.filter((tool) => tool.destructive).map((tool) => tool.name));
    for (const name of ["deploy", "project_remove", "server_remove", "volume_release"]) expect(destructive).toContain(name);
    expect(destructive).not.toContain("publish");
    expect(AGENT_TOOLS.filter((tool) => tool.readOnly && tool.destructive)).toEqual([]);
  });

  it("names at most one other tool in a description: what to use instead, or what comes next", () => {
    const names = AGENT_TOOLS.map((tool) => tool.name).filter((name) => name.includes("_"));
    const offences = AGENT_TOOLS.flatMap((tool) => {
      const named = names.filter((name) => name !== tool.name && new RegExp(`\\b${name}\\b`).test(tool.description));
      return named.length > 1 ? [`${tool.name} names ${named.join(", ")}`] : [];
    });
    expect(offences).toEqual([]);
  });

  it("stops at a skipped Deploy's save, in the place it was given", async () => {
    const { step } = await Effect.runPromise(untilWrite(agentStart("deploy", { skip_deploy: true }, place, "k"), () => Effect.succeed({ value: empty })));
    expect(step).toMatchObject({
      step: "write",
      write: { at: "store", write: { command: "publish", environment: { project: "shop", environment: "production" } } },
    });
  });

  it("hands a Store write to the writer and its refusal back to the model", async () => {
    const written: Write[] = [];
    const write = (command: Write) => Effect.sync((): Answer => {
      written.push(command);
      return { error: { code: "conflict", message: "The Environment moved.", details: null } };
    });
    const result = await Effect.runPromise(finish(agentStart("deploy", { skip_deploy: true }, place, "k"), () => Effect.succeed({ value: empty }), write));
    expect(written).toHaveLength(1);
    expect(result).toMatchObject({ ok: false, refusal: { code: "conflict", message: "The Environment moved." } });
  });
});
