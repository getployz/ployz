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
    const sample = { string: "1", boolean: true, array: ["x=y"] } satisfies Record<string, JsonValue>;
    for (const { command, binding } of AGENT_COMMANDS) {
      const input: JsonValue = Object.fromEntries(Object.entries(inputSchema(command, binding).properties).map(([key, { type }]) => [key, sample[type]]));
      expect(() => (binding.kind === "read" ? binding.query(input) : binding.command(input, "run-1")), command.command).not.toThrow();
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

  it("maps set assignments onto the edit the CLI sends, each value kept as text", () => {
    const set = BINDINGS.get("set");
    if (set?.kind !== "write") throw new Error("set stages");
    expect(set.command({ env: "production", assignment: ["api.image=ghcr.io/acme/api:2.4", "api.env.URL=a=b"], expect: "4" }, "run-1")).toEqual({
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
    expect(() => set.command({ assignment: ["api.image"] }, "run-1")).toThrow("Expected PATH=VALUE, for example web.replicas=3");
    expect(() => set.command({ assignment: ["api.replicas=2", "api.replicas=3"] }, "run-1")).toThrow("api.replicas is given twice; set it once");
    expect(() => set.command({ assignment: ["API.replicas=2", "api.replicas=3"] }, "run-1")).toThrow("api.replicas is given twice; set it once");
    expect(set.command({ assignment: ["volumes.data.name=a", "volumes.DATA.name=b"] }, "run-1")).toMatchObject({ command: "edit" });
  });

  it("refuses a set that expects something other than a revision number", () => {
    const set = BINDINGS.get("set");
    if (set?.kind !== "write") throw new Error("set stages");
    for (const expected of ["not-a-revision", "", "4.5", "-1"]) {
      expect(() => set.command({ assignment: ["api.replicas=2"], expect: expected }, "run-1"), expected).toThrow("expect is a revision number, for example 4");
    }
  });

  it("maps service add onto the create the CLI sends", () => {
    const add = BINDINGS.get("service add");
    if (add?.kind !== "write") throw new Error("service add stages");
    expect(add.command({ name: "cache", image: "redis:7" }, "run-1")).toMatchObject({
      command: "create_service",
      environment: { project: null, environment: null },
      name: "cache",
      image: "redis:7",
    });
    expect(add.command({ name: "empty" }, "run-1")).toMatchObject({ image: null });
  });

  it("keeps a service add's ID when the model retries it in one turn, so the Store replays the first create", () => {
    const add = BINDINGS.get("service add");
    if (add?.kind !== "write") throw new Error("service add stages");
    const id = (input: JsonValue, turn: string) => {
      const command = add.command(input, turn);
      return command.command === "create_service" ? command.id : expect.fail("service add creates a Service");
    };
    const first = id({ name: "cache", image: "redis:7" }, "run-1");
    expect(id({ name: "cache", image: "redis:7" }, "run-1")).toBe(first);
    expect(id({ name: "cache", image: "redis:7" }, "run-2")).not.toBe(first);
    expect(id({ name: "cache", image: "redis:8" }, "run-1")).not.toBe(first);
    expect(first).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-8[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  });
});
