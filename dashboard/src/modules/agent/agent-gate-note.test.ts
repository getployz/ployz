import { describe, expect, it } from "vitest";
import { GATE_NOTE } from "#/modules/agent/agent-chat.server";
import { GATED } from "#/modules/agent/agent-tools";

/** The calls the system prompt names, by the words it uses, and the tools each stands for. */
const NAMED: ReadonlyArray<readonly [phrase: string, tools: readonly string[]]> = [
  ["A Deploy (skip_deploy included)", ["deploy"]],
  ["Retrying a Deployment", ["deployment_retry"]],
  ["removing an Environment or a Project", ["environment_remove", "project_remove"]],
  ["a Service restart or stop", ["service_restart", "service_stop"]],
  ["a server drain, cleanup, removal or forget", ["server_drain", "server_clean", "server_remove", "server_forget"]],
  ["turning web traffic off on a server", ["server_set"]],
  ["a Volume mirror, mirror removal, move or release", ["volume_mirror", "volume_mirror_remove", "volume_move", "volume_release"]],
];

/** Gated tools the prompt leaves to their own description, which ends with the sentence that they may ask. */
const LEFT_TO_DESCRIPTION = [
  "cluster_rename",
  "environment_protect",
  "org_invite_revoke",
  "org_member_rm",
  "org_member_role",
  "private_access_use",
  "volume_restore",
  "web_traffic_use",
];

describe("the system prompt's gate note", () => {
  it("names every gated tool, or the tool is listed as left to its description", () => {
    for (const [phrase] of NAMED) expect(GATE_NOTE).toContain(phrase);
    const named = new Set([...NAMED.flatMap(([, tools]) => tools), ...LEFT_TO_DESCRIPTION]);
    expect([...GATED].filter((tool) => !named.has(tool))).toEqual([]);
    expect([...named].filter((tool) => !GATED.has(tool))).toEqual([]);
  });
});
