import type { JsonValue } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { AGENT_COMMANDS, BINDINGS, COMMANDS, UNBOUND, argKey, inputSchema, toolName } from "./agent-tools";

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

  it("reads only arguments the command declares", () => {
    const undeclared = AGENT_COMMANDS.flatMap(({ command, binding }) =>
      binding.input
        .filter((key) => !command.args.some((declared) => argKey(declared.name) === key))
        .map((key) => `${command.command} ${key}`),
    );
    expect(undeclared).toEqual([]);
  });

  it("parses every input its tool schema advertises", () => {
    const sample = { string: "x", boolean: true, array: ["x=y"] } satisfies Record<string, JsonValue>;
    for (const { command, binding } of AGENT_COMMANDS) {
      const input: JsonValue = Object.fromEntries(Object.entries(inputSchema(command, binding).properties).map(([key, { type }]) => [key, sample[type]]));
      expect(() => (binding.kind === "read" ? binding.query(input) : binding.command(input)), command.command).not.toThrow();
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
    expect(command.command({ env: "production", service: ["api"], accept_volume_loss: ["pg-data"], expect_version: "v7" })).toMatchObject({
      command: "admit",
      admit: "deploy",
      environment: { project: null, environment: "production" },
      services: ["api"],
      version: "v7",
      accept_volume_loss: ["pg-data"],
    });
  });

  it("maps set assignments onto the edit the CLI sends, each value kept as text", () => {
    const set = BINDINGS.get("set");
    if (set?.kind !== "write") throw new Error("set stages");
    expect(set.command({ env: "production", assignment: ["api.image=ghcr.io/acme/api:2.4", "api.env.URL=a=b"], expect: "4" })).toEqual({
      command: "edit",
      environment: { project: null, environment: "production" },
      expect: 4,
      changes: [
        { op: "set", path: "api.image", value: "ghcr.io/acme/api:2.4" },
        { op: "set", path: "api.env.URL", value: "a=b" },
      ],
    });
  });

  it("refuses a set that names no value or one Setting twice, as the CLI does", () => {
    const set = BINDINGS.get("set");
    if (set?.kind !== "write") throw new Error("set stages");
    expect(() => set.command({ assignment: ["api.image"] })).toThrow("Expected PATH=VALUE, for example web.replicas=3");
    expect(() => set.command({ assignment: ["api.replicas=2", "api.replicas=3"] })).toThrow("api.replicas is given twice; set it once");
  });

  it("maps service add onto the create the CLI sends", () => {
    const add = BINDINGS.get("service add");
    if (add?.kind !== "write") throw new Error("service add stages");
    expect(add.command({ name: "cache", image: "redis:7" })).toMatchObject({
      command: "create_service",
      environment: { project: null, environment: null },
      name: "cache",
      image: "redis:7",
    });
    expect(add.command({ name: "empty" })).toMatchObject({ image: null });
  });
});
