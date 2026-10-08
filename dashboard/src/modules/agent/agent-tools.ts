import type { ConfigCommand, ConfigQuery, EnvironmentRef, JsonValue } from "@ployz/sdk";
import { Schema } from "effect";
import commandsJson from "../../../../core/crates/ployz-sdk/generated/commands.json";

const CatalogArg = Schema.Struct({
  name: Schema.String,
  required: Schema.Boolean,
  type: Schema.String,
  multiple: Schema.Boolean,
  help: Schema.optional(Schema.String),
});
const CatalogCommand = Schema.Struct({
  command: Schema.String,
  about: Schema.String,
  surface: Schema.Literals(["cloud", "local", "internal"]),
  approval: Schema.Literals(["never", "always", "depends"]),
  args: Schema.Array(CatalogArg),
});
export type CatalogCommand = typeof CatalogCommand.Type;
type CatalogArg = typeof CatalogArg.Type;

/** `commands.json`, the catalog `ployz mcp` serves, generated from the CLI's clap tree. */
export const COMMANDS = Schema.decodeUnknownSync(Schema.Array(CatalogCommand))(commandsJson);

const Text = Schema.optional(Schema.String);
const Names = Schema.optional(Schema.Array(Schema.String));
const Flag = Schema.optional(Schema.Boolean);
const Env = { project: Text, env: Text };

/**
 * How the Agent sidebar runs one command: the Store query or command the CLI sends for it. `input` names the
 * command's arguments by their tool keys. A `gated` write is a Publish or Deploy, which asks a human when its plan
 * destroys something. Each binding parses its own tool input, which the model writes and nothing has checked.
 */
export type AgentBinding =
  | { kind: "read"; input: readonly string[]; query: (input: JsonValue) => ConfigQuery }
  | { kind: "write" | "gated"; input: readonly string[]; command: (input: JsonValue) => ConfigCommand };

type InputSchema = Schema.ConstraintDecoder<object> & { readonly fields: Schema.Struct.Fields };

const environment = (input: { readonly project?: string; readonly env?: string }): EnvironmentRef => ({
  project: input.project ?? null,
  environment: input.env ?? null,
});

const read = <S extends InputSchema>(input: S, query: (input: S["Type"]) => ConfigQuery): AgentBinding => {
  const decode = Schema.decodeUnknownSync(input);
  return { kind: "read", input: Object.keys(input.fields), query: (raw) => query(decode(raw)) };
};
const write =
  (kind: "write" | "gated") =>
  <S extends InputSchema>(input: S, command: (input: S["Type"]) => ConfigCommand): AgentBinding => {
    const decode = Schema.decodeUnknownSync(input);
    return { kind, input: Object.keys(input.fields), command: (raw) => command(decode(raw)) };
  };
const stage = write("write");
const gate = write("gated");

export const BINDINGS = new Map<string, AgentBinding>([
  ["project ls", read(Schema.Struct({}), () => ({ query: "projects" }))],
  ["env ls", read(Schema.Struct({ project: Text }), (input) => ({ query: "environments", project: input.project ?? null }))],
  ["service ls", read(Schema.Struct(Env), (input) => ({ query: "services", environment: environment(input) }))],
  [
    "service inspect",
    read(Schema.Struct({ ...Env, service: Schema.String }), (input) => ({ query: "service", environment: environment(input), service: input.service })),
  ],
  ["volume ls", read(Schema.Struct(Env), (input) => ({ query: "volumes", environment: environment(input) }))],
  [
    "domain ls",
    read(Schema.Struct({ ...Env, service: Text }), (input) => ({ query: "domains", environment: environment(input), service: input.service ?? null })),
  ],
  [
    "deployment ls",
    read(Schema.Struct({ ...Env, limit: Text }), (input) => ({
      query: "deployments",
      environment: environment(input),
      limit: input.limit === undefined ? null : Number(input.limit),
      cursor: null,
    })),
  ],
  ["deployment show", read(Schema.Struct({ id: Schema.String }), (input) => ({ query: "deployment", id: input.id }))],
  ["diff", read(Schema.Struct(Env), (input) => ({ query: "diff", environment: environment(input) }))],
  [
    "get",
    read(Schema.Struct({ ...Env, path: Text, all: Flag }), (input) => ({
      query: "environment",
      environment: environment(input),
      path: input.path ?? null,
      all: input.all ?? false,
    })),
  ],
  [
    "service rm",
    stage(Schema.Struct({ ...Env, service: Schema.String }), (input) => ({
      command: "remove_service",
      environment: environment(input),
      service: input.service,
    })),
  ],
  [
    "volume rm",
    stage(Schema.Struct({ ...Env, volume: Schema.String }), (input) => ({ command: "remove_volume", environment: environment(input), volume: input.volume })),
  ],
  [
    "discard",
    stage(Schema.Struct({ ...Env, path: Text, version: Text }), (input) => ({
      command: "discard",
      environment: environment(input),
      path: input.path ?? null,
      version: input.version ?? null,
    })),
  ],
  ["publish", gate(Schema.Struct({ ...Env, version: Text }), (input) => ({ command: "publish", environment: environment(input), version: input.version ?? null }))],
  [
    "deploy",
    gate(Schema.Struct({ ...Env, service: Names, expect_version: Text, message: Text, accept_volume_loss: Names }), (input) => ({
      command: "admit",
      admit: "deploy",
      id: crypto.randomUUID(),
      environment: environment(input),
      services: [...(input.service ?? [])],
      version: input.expect_version ?? null,
      message: input.message ?? null,
      accept_volume_loss: [...(input.accept_volume_loss ?? [])],
    })),
  ],
]);

/** Why each other Cloud command has no tool yet: nothing here is hidden by accident. */
export const UNBOUND = new Map<string, string>([
  ["cloud reset", "resets a Server's Cloud pairing on the Server itself"],
  ["deployment cancel", "not yet bound"],
  ["deployment retry", "not yet bound"],
  ["deployment start", "not yet bound"],
  ["domain add", "not yet bound"],
  ["domain check", "needs DNS lookups the CLI gathers"],
  ["domain rm", "not yet bound"],
  ["domain set", "not yet bound"],
  ["env branch", "not yet bound"],
  ["env copy", "not yet bound"],
  ["env default", "not yet bound"],
  ["env keep", "not yet bound"],
  ["env never-sync", "not yet bound"],
  ["env new", "not yet bound"],
  ["env pr", "talks to GitHub"],
  ["env rm", "deletes an Environment, which the dashboard confirms by typing its name"],
  ["env setup", "not yet bound"],
  ["env shutdown", "not yet bound"],
  ["env sync", "not yet bound"],
  ["exec", "needs a terminal on a Server"],
  ["explain", "reads the local Setting catalog"],
  ["github connect", "opens a browser"],
  ["github disconnect", "talks to GitHub"],
  ["github ls", "talks to GitHub"],
  ["logs", "streams from Servers"],
  ["org build-order", "not yet bound"],
  ["org ls", "the sidebar acts in the Organization you are in"],
  ["org rm", "deletes an Organization, which the dashboard confirms by typing its name"],
  ["org use", "the dashboard has one Organization switch"],
  ["project new", "not yet bound"],
  ["project rename", "not yet bound"],
  ["project rm", "deletes a Project, which the dashboard confirms by typing its name"],
  ["ps", "reads containers from Servers"],
  ["schema", "reads the local Setting catalog"],
  ["server add", "installs ployz over SSH from your machine"],
  ["server clean", "runs on Servers through Cloud workflows"],
  ["server drain", "runs on Servers through Cloud workflows"],
  ["server forget", "not yet bound"],
  ["server inspect", "reads from Servers"],
  ["server logs", "streams from Servers"],
  ["server ls", "reads from Servers"],
  ["server rm", "removes a Server, which the dashboard confirms by typing its name"],
  ["server set", "not yet bound"],
  ["server upgrade", "runs on Servers through Cloud workflows"],
  ["service add", "not yet bound"],
  ["service port-forward", "needs a local port"],
  ["service rename", "not yet bound"],
  ["service restart", "acts on running containers"],
  ["service start", "acts on running containers"],
  ["service stop", "acts on running containers"],
  ["set", "an assignment's parsing lives in the CLI"],
  ["status", "joins Store and Server state the CLI gathers"],
  ["token ls", "Organization Tokens live in the Organization's settings"],
  ["token new", "a token's secret must never pass through a chat"],
  ["token rm", "Organization Tokens live in the Organization's settings"],
  ["unset", "a path's parsing lives in the CLI"],
  ["up", "uploads local source"],
  ["volume add", "not yet bound"],
  ["volume inspect", "not yet bound"],
  ["volume mirror", "runs on Servers through Cloud workflows"],
  ["volume mirror rm", "runs on Servers through Cloud workflows"],
  ["volume rename", "not yet bound"],
  ["volume runs", "not yet bound"],
  ["volume set", "not yet bound"],
  ["volume sync", "runs on Servers through Cloud workflows"],
]);

/** `--accept-volume-loss` and `SERVICE` as tool input keys: `accept_volume_loss`, `service`. */
export const argKey = (name: string) => name.replace(/^--/, "").replaceAll("-", "_").toLowerCase();

export const toolName = (command: string) => command.replaceAll(" ", "_").replaceAll("-", "_");

type JsonProperty =
  | { type: "string"; description?: string }
  | { type: "boolean"; description?: string }
  | { type: "array"; items: { type: "string" }; description?: string };
type JsonSchema = { type: "object"; properties: Record<string, JsonProperty>; required: string[]; additionalProperties: false };

const property = (arg: CatalogArg): JsonProperty => {
  const description = arg.help === undefined ? {} : { description: arg.help };
  if (arg.type === "boolean") return { type: "boolean", ...description };
  return arg.multiple ? { type: "array", items: { type: "string" }, ...description } : { type: "string", ...description };
};

/** A command's tool input: the catalog's wording of each argument its binding reads. */
export function inputSchema(command: CatalogCommand, binding: AgentBinding): JsonSchema {
  const args = command.args.filter((arg) => binding.input.includes(argKey(arg.name)));
  return {
    type: "object",
    properties: Object.fromEntries(args.map((arg) => [argKey(arg.name), property(arg)])),
    required: args.filter((arg) => arg.required).map((arg) => argKey(arg.name)),
    additionalProperties: false,
  };
}

/** Every bound Cloud command with its catalog entry: the sidebar's tool set. */
export const AGENT_COMMANDS = COMMANDS.flatMap((command) => {
  const binding = BINDINGS.get(command.command);
  return command.surface === "cloud" && binding !== undefined ? [{ command, binding }] : [];
});
