import { type Answer, type Write, agentStart } from "@ployz/sdk";
import { Effect } from "effect";
import { describe, expect, it } from "vitest";
import { COMMANDS } from "#/modules/commands/registry.server";
import { AGENT_TOOLS } from "./agent-runner.server";
import { CATALOG, GATED, finish, untilWrite } from "./agent-tools";

const place = { project: "shop", environment: "production", store: { at: "cloud", organization: "org" } } as const;
const empty = { view: "services", services: [] };

describe("agent tools", () => {
  it("gates every tool whose write may ask a human, and no other", () => {
    expect(GATED).toEqual(new Set([
      ...CATALOG.tools.filter((tool) => tool.gate !== "never").map((tool) => tool.name),
      ...COMMANDS.filter((command) => command.gate !== "never").map((command) => command.name),
    ]));
    expect(GATED).toContain("deploy");
    expect(GATED).not.toContain("publish");
    expect(GATED).not.toContain("environment_show");
  });

  it("marks destructive exactly the tools that delete something when they run, and never a read", () => {
    expect(AGENT_TOOLS.filter((tool) => tool.destructive).map((tool) => tool.name).sort()).toEqual([
      "deploy",
      "deployment_retry",
      "deployment_start",
      "discard",
      "environment_remove",
      "org_invite_revoke",
      "org_member_rm",
      "project_remove",
      "server_clean",
      "server_drain",
      "server_forget",
      "server_remove",
      "volume_mirror",
      "volume_mirror_remove",
      "volume_move",
      "volume_release",
    ]);
    expect(AGENT_TOOLS.filter((tool) => tool.readOnly && tool.destructive)).toEqual([]);
  });

  it("never names another tool in a description", () => {
    const names = AGENT_TOOLS.map((tool) => tool.name).filter((name) => name.includes("_"));
    const offences = AGENT_TOOLS.flatMap((tool) =>
      names.filter((name) => name !== tool.name && new RegExp(`\\b${name}\\b`).test(tool.description)).map((name) => `${tool.name} names ${name}`),
    );
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
