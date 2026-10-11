import type { JsonValue } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import type { Caller } from "#/modules/identity/actor";
import { AGENT_COMMANDS, type AgentBinding, BINDINGS, COMMANDS, UNBOUND, argKey, inputSchema, toolName } from "./agent-tools";

const caller = { userId: "u", organization: { id: "o", slug: "acme" }, credential: { kind: "session", id: "s" } } as Caller;

const parse = (binding: AgentBinding, input: JsonValue) => {
  if (binding.kind === "read") return binding.query(input);
  if (binding.kind === "cloud") return binding.run(input, caller, "run-1");
  return binding.command(input, "run-1");
};

const cloud = COMMANDS.filter((command) => command.surface === "cloud").map((command) => command.command);

describe("agent tools", () => {
  it("accounts for every Cloud command as a tool or a reason", () => {
    const missing = cloud.filter((command) => !BINDINGS.has(command) && !UNBOUND.has(command));
    expect(missing).toEqual([]);
  });

  it("binds and excuses only Cloud commands", () => {
    const named = [...BINDINGS.keys(), ...UNBOUND.keys()];
    expect(named.filter((command) => !cloud.includes(command))).toEqual([]);
    expect([...BINDINGS.keys()].filter((command) => UNBOUND.has(command))).toEqual([]);
  });

  it("exposes no Local or Internal command", () => {
    const surfaces = new Set(AGENT_COMMANDS.map(({ command }) => command.surface));
    expect([...surfaces]).toEqual(["cloud"]);
    expect(AGENT_COMMANDS.map(({ command }) => command.command)).not.toContain("server add");
  });

  it("reads only arguments the command declares, and the input it adds", () => {
    const undeclared = AGENT_COMMANDS.flatMap(({ command, binding }) =>
      binding.input
        .filter((key) => !command.args.some((declared) => argKey(declared.name) === key) && !(key in (binding.added ?? {})))
        .map((key) => `${command.command} ${key}`),
    );
    expect(undeclared).toEqual([]);
  });

  it("parses every input its tool schema advertises", () => {
    const sample = { string: "1", integer: 1, boolean: true, array: ["x=y"] } satisfies Record<string, JsonValue>;
    for (const { command, binding } of AGENT_COMMANDS) {
      const properties = Object.entries(inputSchema(command, binding).properties)
        .filter(([key]) => !(command.command === "service add" && key === "image"));
      const input: JsonValue = Object.fromEntries(properties.map(([key, { type }]) => [key, key === "mount" ? ["web:/etc/web"] : sample[type]]));
      expect(() => parse(binding, input), command.command).not.toThrow();
    }
  });

  it("gates exactly Publish and Deploy", () => {
    const gated = AGENT_COMMANDS.filter(({ binding }) => binding.kind === "gated").map(({ command }) => command.command);
    expect(gated.sort()).toEqual(["deploy", "publish"]);
  });

  it("lets the model accept volume loss on a Deploy at a version", () => {
    const deploy = AGENT_COMMANDS.find(({ command }) => command.command === "deploy");
    if (deploy === undefined) throw new Error("deploy is bound");
    const schema = inputSchema(deploy.command, deploy.binding);
    expect(schema.properties).toMatchObject({ accept_volume_loss: { type: "array" }, expect_version: { type: "string" } });
    expect(toolName(deploy.command.command)).toBe("deploy");
  });

  it("maps Deploy arguments onto the admit command the CLI sends", () => {
    const command = BINDINGS.get("deploy");
    if (command?.kind !== "gated") throw new Error("deploy is gated");
    expect(command.command({ env: "production", service: ["api"], accept_volume_loss: ["pg-data"], expect_version: "v7" }, "run-1")).toMatchObject({
      command: "admit",
      admit: "deploy",
      environment: { project: null, environment: "production" },
      services: ["api"],
      version: "v7",
      accept_volume_loss: ["pg-data"],
    });
  });

  it("maps Publish arguments onto the publish the CLI sends, accepting named Volume loss at a version", () => {
    const publish = BINDINGS.get("publish");
    if (publish?.kind !== "gated") throw new Error("publish is gated");
    expect(publish.command({ env: "production", accept_volume_loss: ["pg-data"], version: "9:1:0.1:abc" }, "run-1")).toEqual({
      command: "publish",
      environment: { project: null, environment: "production" },
      version: "9:1:0.1:abc",
      accept_volume_loss: ["pg-data"],
    });
    expect(publish.command({}, "run-1")).toMatchObject({ version: null, accept_volume_loss: [] });
  });

  it("asks for a Config file's text inline, never a path", () => {
    const put = AGENT_COMMANDS.find(({ command }) => command.command === "config put");
    if (put === undefined) throw new Error("config put is bound");
    const schema = inputSchema(put.command, put.binding);
    expect(schema.required).toEqual(expect.arrayContaining(["config", "file", "content"]));
    expect(Object.keys(schema.properties)).not.toContain("from");
    expect(schema.properties["uid"]).toMatchObject({ type: "integer" });
  });

  it("leaves github disconnect to a human, and github connect takes no input", () => {
    const connect = AGENT_COMMANDS.find(({ command }) => command.command === "github connect");
    if (connect === undefined) throw new Error("github connect is bound");
    expect(inputSchema(connect.command, connect.binding).properties).toEqual({});
    expect(UNBOUND.get("github disconnect")).toBe("asks a human; bound with the other always-approval commands");
  });
});
