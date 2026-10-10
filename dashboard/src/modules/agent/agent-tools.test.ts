import type { ConfigCommand, JsonValue } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import type { Caller } from "#/modules/identity/actor";
import { AGENT_COMMANDS, type AgentBinding, BINDINGS, COMMANDS, UNBOUND, argKey, inputSchema, toolName } from "./agent-tools";

const caller = { userId: "u", organization: { id: "o", slug: "acme" }, credential: { kind: "session", id: "s" } } as Caller;

/** `command`'s staged write, for `input` in `turn`. */
const staged = (command: string, input: JsonValue, turn = "run-1"): ConfigCommand => {
  const binding = BINDINGS.get(command);
  if (binding?.kind !== "write") throw new Error(`${command} stages`);
  return binding.command(input, turn);
};

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

  it("adds a Git Service from a repository, at a branch after @, and never with an image too", () => {
    expect(staged("service add", { name: "web", repo: "acme/web@dev" })).toMatchObject({
      command: "create_git_service", environment: { project: null, environment: null }, name: "web", repository: "acme/web", branch: "dev",
    });
    expect(staged("service add", { name: "web", repo: "acme/web" })).toMatchObject({ repository: "acme/web", branch: null });
    expect(() => staged("service add", { name: "web", repo: "acme/web", image: "nginx" })).toThrow("Give image or repo, not both");
  });

  it("keeps a new Project's and its Default Environment's IDs within one turn, and only there", () => {
    const ids = (turn: string) => {
      const command = staged("project new", { name: "shop" }, turn);
      return command.command === "create_project" ? [command.id, command.default_environment] : expect.fail("project new creates a Project");
    };
    const [project, environment] = ids("run-1");
    expect(project).not.toBe(environment);
    expect(ids("run-1")).toEqual([project, environment]);
    expect(ids("run-2")).not.toContain(project);
  });

  it("puts a Config file's text as the model gives it", () => {
    expect(staged("config put", { env: "production", config: "nginx", file: "conf.d/site.conf", content: "server {}\n", executable: true, uid: 101 }))
      .toEqual({
        command: "put_config_file",
        environment: { project: null, environment: "production" },
        config: "nginx",
        file: "conf.d/site.conf",
        content: "server {}\n",
        mode: "0555",
        uid: 101,
        gid: null,
      });
    const put = AGENT_COMMANDS.find(({ command }) => command.command === "config put");
    if (put === undefined) throw new Error("config put is bound");
    const schema = inputSchema(put.command, put.binding);
    expect(schema.required).toEqual(expect.arrayContaining(["config", "file", "content"]));
    expect(Object.keys(schema.properties)).not.toContain("from");
    expect(schema.properties["uid"]).toMatchObject({ type: "integer" });
  });

  it("creates Configs, mounts, domains, Environments and setups as the CLI sends them", () => {
    expect(staged("config add", { name: "sentry", mount: ["web:/etc/sentry"] })).toMatchObject({
      command: "create_config", name: "sentry", mounts: [{ service: "web", dir: "/etc/sentry" }],
    });
    expect(staged("config mount", { service: "web", config: "nginx", dir: "/etc/nginx" })).toMatchObject({ command: "attach_config", dir: "/etc/nginx" });
    expect(staged("domain add", { service: "web", host: " App.Example.com ", port: 8080 })).toMatchObject({
      command: "add_domain", hostname: "app.example.com", port: 8080,
    });
    expect(staged("domain add", { service: "web" })).toMatchObject({ hostname: null, port: null });
    expect(staged("env new", { name: "staging", project: "shop" })).toMatchObject({ command: "create_environment", project: "shop", name: "staging" });
    expect(staged("env setup", { setup: ["web=pnpm db:seed"] })).toMatchObject({
      command: "set_branch_setup", setup: [{ service: "web", command: "pnpm db:seed" }],
    });
    expect(staged("env setup", { clear: true })).toMatchObject({ setup: [] });
    expect(() => staged("env setup", {})).toThrow("Give setup SERVICE=COMMAND, or clear");
  });

  it("binds GitHub reads and env pr as Cloud commands, and leaves disconnect to a human", () => {
    const kinds = Object.fromEntries(["github ls", "github connect", "github tree", "github cat", "env pr", "deployment start"]
      .map((command) => [command, BINDINGS.get(command)?.kind]));
    expect(kinds).toEqual({
      "github ls": "cloud", "github connect": "cloud", "github tree": "cloud", "github cat": "cloud", "env pr": "cloud", "deployment start": "cloud",
    });
    const connect = AGENT_COMMANDS.find(({ command }) => command.command === "github connect");
    if (connect === undefined) throw new Error("github connect is bound");
    expect(inputSchema(connect.command, connect.binding).properties).toEqual({});
    expect(UNBOUND.get("github disconnect")).toBe("asks a human; bound with the other always-approval commands");
  });
});
