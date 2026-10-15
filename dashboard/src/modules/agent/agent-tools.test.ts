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

  it("marks destructive exactly the tools that remove something, and never a read", () => {
    expect(AGENT_TOOLS.filter((tool) => tool.destructive).map((tool) => tool.name).sort()).toEqual([
      "config_remove",
      "deploy",
      "discard",
      "domain_remove",
      "environment_remove",
      "org_invite_revoke",
      "org_member_rm",
      "project_remove",
      "server_clean",
      "server_forget",
      "server_remove",
      "service_remove",
      "volume_mirror_remove",
      "volume_remove",
    ]);
    expect(AGENT_TOOLS.filter((tool) => tool.readOnly && tool.destructive)).toEqual([]);
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
