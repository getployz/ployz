import { createHash } from "node:crypto";
import type { Change, ConfigCommand, ConfigQuery, EnvironmentRef, JsonValue, SetupCommand } from "@ployz/sdk";
import { Effect, Schema } from "effect";
import commandsJson from "../../../../core/crates/ployz-sdk/generated/commands.json";
import { callStore } from "#/modules/config-store/config-store.server";
import { type StoreCall, StoreRefused } from "#/modules/config-store/store.contract";
import { githubBranches, githubConnection, GithubRepositories } from "#/modules/github/github-cli.server";
import type { Caller } from "#/modules/identity/actor";
import type { Validation } from "#/server/public-error";

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
const Int = Schema.optional(Schema.Int);
const Env = { project: Text, env: Text };

/** The Store source the sidebar's calls are recorded under. */
export const AGENT = { source: "agent" } as const;

type CloudEffect =
  | ReturnType<typeof githubConnection>
  | ReturnType<typeof githubBranches>
  | ReturnType<GithubRepositories["Service"]["tree"]>
  | ReturnType<typeof callStore<StoreCall>>;
export type CloudServices = Effect.Services<CloudEffect> | GithubRepositories;
/** How a `cloud` binding refuses: `NotFound`, `Validation` and a Store refusal reach the model as its refusal. */
export type CloudError = Effect.Error<CloudEffect> | Validation | StoreRefused;

/**
 * How the Agent sidebar runs one command: the Store query or command the CLI sends for it, or for a `cloud` command,
 * what Cloud answers outside one Store call. `input` names the command's arguments by their tool keys; `added` is input
 * the CLI reads from elsewhere, like a file's text. A `gated` write is a Publish or Deploy, which asks a human when its
 * plan destroys something. Each binding parses its own tool input, which the model writes and nothing has checked.
 * `turn` names the run calling it, so a create the model retries in one turn keeps its ID and the Store replays it.
 */
export type AgentBinding = { input: readonly string[]; added?: Readonly<Record<string, JsonProperty>> } & (
  | { kind: "read"; query: (input: JsonValue) => ConfigQuery }
  | { kind: "write" | "gated"; command: (input: JsonValue, turn: string) => ConfigCommand }
  | { kind: "cloud"; run: (input: JsonValue, caller: Caller, turn: string) => Effect.Effect<unknown, CloudError, CloudServices> }
);
export type StoreBinding = Exclude<AgentBinding, { kind: "cloud" }>;

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
  <S extends InputSchema>(input: S, command: (input: S["Type"], turn: string) => ConfigCommand): AgentBinding => {
    const decode = Schema.decodeUnknownSync(input);
    return { kind, input: Object.keys(input.fields), command: (raw, turn) => command(decode(raw), turn) };
  };
const stage = write("write");
const gate = write("gated");
const cloud = <S extends InputSchema>(
  input: S,
  run: (input: S["Type"], caller: Caller, turn: string) => Effect.Effect<unknown, CloudError, CloudServices>,
): AgentBinding => {
  const decode = Schema.decodeUnknownSync(input);
  return { kind: "cloud", input: Object.keys(input.fields), run: (raw, caller, turn) => run(decode(raw), caller, turn) };
};

/** One Store call as the member, its refusal failing the binding. */
const stored = <C extends StoreCall>(caller: Caller, call: C) =>
  callStore(caller.organization.id, caller.userId, call, AGENT).pipe(
    Effect.flatMap((result) => result.ok ? Effect.succeed(result.value) : Effect.fail(new StoreRefused(result.refusal))));

const usage = (message: string) => new StoreRefused({ code: "invalid_argument", message, details: null });

/** A UUID that `seed` always yields, for a Store ID that must come out the same when its command is sent again. */
export const uuidFrom = (seed: string) => {
  const hex = createHash("sha256").update(seed).digest("hex");
  const variant = ((Number.parseInt(hex[16] ?? "0", 16) & 0x3) | 0x8).toString(16);
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-8${hex.slice(13, 16)}-${variant}${hex.slice(17, 20)}-${hex.slice(20, 32)}`;
};

/** `path` as the Store names it: a Service's name is lowercase, so `WEB.replicas` is `web.replicas`. */
const canonical = (path: string) => {
  if (path === "volumes" || path.startsWith("volumes.")) return path;
  const dot = path.indexOf(".");
  return dot === -1 ? path.toLowerCase() : path.slice(0, dot).toLowerCase() + path.slice(dot);
};

/** `web.replicas=3` as the Change the CLI's `set` sends: split at the first `=`, the value kept as text. */
const assignments = (given: ReadonlyArray<string>): Change[] => {
  const changes = given.map((assignment): Change => {
    const split = assignment.indexOf("=");
    if (split === -1) throw new Error("Expected PATH=VALUE, for example web.replicas=3");
    return { op: "set", path: assignment.slice(0, split), value: assignment.slice(split + 1) };
  });
  const paths = changes.map((change) => canonical(change.path));
  const repeated = paths.find((path, index) => paths.indexOf(path) !== index);
  if (repeated !== undefined) throw new Error(`${repeated} is given twice; set it once`);
  return changes;
};

/** `--expect 4`: the Working revision an edit must still find, never a guess when the model wrote something else. */
const revision = (expect: string) => {
  if (!/^\d+$/.test(expect)) throw new Error("expect is a revision number, for example 4");
  return Number(expect);
};

/** `web=pnpm db:seed` as the setup the CLI sends: split at the first `=`. */
const setups = (given: ReadonlyArray<string>): SetupCommand[] => given.map((setup) => {
  const split = setup.indexOf("=");
  if (split <= 0) throw new Error("Expected SERVICE=COMMAND, for example web=pnpm db:seed");
  return { service: setup.slice(0, split), command: setup.slice(split + 1) };
});

/** `acme/web@dev` as the repository and branch `service add --repo` sends. */
const gitSource = (repo: string) => {
  const at = repo.indexOf("@");
  return at === -1 ? { repository: repo, branch: null } : { repository: repo.slice(0, at), branch: repo.slice(at + 1) };
};

/** `#5` or `5`: a Deployment's number in its Environment, which the Store resolves; anything else is its ID. */
const deploymentNumber = (given: string) => /^#?\d+$/.test(given) ? Number(given.replace("#", "")) : null;

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
    read(Schema.Struct({ ...Env, limit: Int }), (input) => ({
      query: "deployments",
      environment: environment(input),
      limit: input.limit ?? null,
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
    "set",
    stage(Schema.Struct({ ...Env, assignment: Schema.Array(Schema.String), expect: Text }), (input) => ({
      command: "edit",
      environment: environment(input),
      expect: input.expect === undefined ? null : revision(input.expect),
      changes: assignments(input.assignment),
    })),
  ],
  [
    "service add",
    stage(Schema.Struct({ ...Env, name: Schema.String, image: Text, repo: Text }), (input, turn): ConfigCommand => {
      if (input.image !== undefined && input.repo !== undefined) throw new Error("Give image or repo, not both");
      const id = uuidFrom(JSON.stringify(["ployz.agent.service", turn, input.project ?? null, input.env ?? null, input.name, input.image ?? input.repo ?? null]));
      if (input.repo === undefined) {
        return { command: "create_service", id, environment: environment(input), name: input.name, image: input.image ?? null };
      }
      return { command: "create_git_service", id, environment: environment(input), name: input.name, ...gitSource(input.repo) };
    }),
  ],
  [
    "project new",
    stage(Schema.Struct({ name: Schema.String }), (input, turn) => ({
      command: "create_project",
      id: uuidFrom(JSON.stringify(["ployz.agent.project", turn, input.name])),
      name: input.name,
      default_environment: uuidFrom(JSON.stringify(["ployz.agent.project.environment", turn, input.name])),
    })),
  ],
  [
    "env new",
    stage(Schema.Struct({ name: Schema.String, project: Text }), (input, turn) => ({
      command: "create_environment",
      id: uuidFrom(JSON.stringify(["ployz.agent.environment", turn, input.project ?? null, input.name])),
      project: input.project ?? null,
      name: input.name,
    })),
  ],
  [
    "env setup",
    stage(Schema.Struct({ ...Env, setup: Names, clear: Flag }), (input) => {
      if (input.clear !== true && input.setup === undefined) throw new Error("Give setup SERVICE=COMMAND, or clear");
      return { command: "set_branch_setup", environment: environment(input), setup: input.clear === true ? [] : setups(input.setup ?? []) };
    }),
  ],
  [
    "env pr",
    cloud(Schema.Struct({
      repository: Text, project: Text, on: Flag, off: Flag, from: Text, copy: Names, setup: Names, remove_on_close: Flag, bots: Flag,
    }), (input, caller) => Effect.gen(function* () {
      const project = input.project ?? null;
      const enabled = input.on === true ? true : input.off === true ? false : null;
      const setup = input.setup === undefined ? null : yield* Effect.try({ try: () => setups(input.setup ?? []), catch: (error) => usage(error instanceof Error ? error.message : String(error)) });
      const changing = enabled !== null || input.from !== undefined || input.copy !== undefined || setup !== null
        || input.remove_on_close !== undefined || input.bots !== undefined;
      if (!changing) return yield* stored(caller, { operation: "read", query: { query: "pr_plans", project } });
      let repository = input.repository;
      if (repository === undefined) {
        const { plans } = yield* stored(caller, { operation: "read", query: { query: "pr_plans", project } });
        const [only, ...more] = plans;
        if (only === undefined || more.length > 0) return yield* usage("Name the repository: env_pr lists the Project's");
        repository = only.repository;
      }
      return yield* stored(caller, {
        operation: "write",
        command: {
          command: "set_pr_plan",
          project,
          repository,
          enabled,
          start_from: input.from ?? null,
          copy: input.copy === undefined ? null : [...input.copy],
          setup,
          remove_on_close: input.remove_on_close ?? null,
          include_bots: input.bots ?? null,
        },
      });
    })),
  ],
  [
    "config add",
    stage(Schema.Struct({ ...Env, name: Schema.String, mount: Names }), (input, turn) => ({
      command: "create_config",
      id: uuidFrom(JSON.stringify(["ployz.agent.config", turn, input.project ?? null, input.env ?? null, input.name])),
      environment: environment(input),
      name: input.name,
      mounts: (input.mount ?? []).map((mount) => {
        const split = mount.indexOf(":");
        if (split <= 0) throw new Error("Expected SERVICE:/DIR, for example web:/etc/sentry");
        return { service: mount.slice(0, split), dir: mount.slice(split + 1) };
      }),
    })),
  ],
  [
    "config put",
    {
      ...stage(
        Schema.Struct({ ...Env, config: Schema.String, file: Schema.String, content: Schema.String, mode: Text, executable: Flag, uid: Int, gid: Int }),
        (input) => ({
          command: "put_config_file",
          environment: environment(input),
          config: input.config,
          file: input.file,
          content: input.content,
          mode: input.executable === true ? "0555" : input.mode ?? null,
          uid: input.uid ?? null,
          gid: input.gid ?? null,
        }),
      ),
      added: { content: { type: "string", description: "The file's whole text, UTF-8, at most 256 KiB" } },
    },
  ],
  [
    "config mount",
    stage(Schema.Struct({ ...Env, service: Schema.String, config: Schema.String, dir: Schema.String }), (input) => ({
      command: "attach_config",
      environment: environment(input),
      service: input.service,
      config: input.config,
      dir: input.dir,
    })),
  ],
  [
    "domain add",
    stage(Schema.Struct({ ...Env, service: Schema.String, host: Text, port: Int }), (input) => ({
      command: "add_domain",
      environment: environment(input),
      service: input.service,
      hostname: input.host === undefined ? null : input.host.trim().toLowerCase(),
      port: input.port ?? null,
    })),
  ],
  [
    "deployment start",
    cloud(Schema.Struct({ ...Env, id: Schema.String }), (input, caller) => Effect.gen(function* () {
      const number = deploymentNumber(input.id);
      const deployment = number === null
        ? input.id
        : (yield* stored(caller, { operation: "read", query: { query: "numbered_deployment", environment: environment(input), number } })).id;
      return yield* stored(caller, { operation: "write", command: { command: "start", deployment } });
    })),
  ],
  ["github ls", cloud(Schema.Struct({ repository: Text }), (input, caller) =>
    input.repository === undefined ? githubConnection(caller) : githubBranches(caller, input.repository))],
  [
    "github connect",
    cloud(Schema.Struct({}), (_input, caller) => Effect.gen(function* () {
      const connection = yield* githubConnection(caller);
      if (!connection.linked) {
        return yield* new StoreRefused({
          code: "unsupported",
          message: "GitHub reports installs by GitHub account, and yours isn't linked: sign in to Ployz Cloud with GitHub once, then connect again.",
          details: null,
        });
      }
      return { status: connection.ready ? "connected" : "pending", url: connection.install_url };
    })),
  ],
  ["github tree", cloud(Schema.Struct({ repository: Schema.String, path: Text, ref: Text, match: Text }), (input, caller) =>
    Effect.gen(function* () {
      return yield* (yield* GithubRepositories).tree(caller, input);
    }))],
  ["github cat", cloud(Schema.Struct({ repository: Schema.String, path: Schema.String, ref: Text }), (input, caller) =>
    Effect.gen(function* () {
      return yield* (yield* GithubRepositories).file(caller, input);
    }))],
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
  ["publish", gate(Schema.Struct({ ...Env, version: Text, accept_volume_loss: Names }), (input) => ({
    command: "publish", environment: environment(input), version: input.version ?? null, accept_volume_loss: [...(input.accept_volume_loss ?? [])],
  }))],
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
  ["config inspect", "not yet bound"],
  ["config ls", "not yet bound"],
  ["config rename", "not yet bound"],
  ["config rm", "not yet bound"],
  ["config rm-file", "not yet bound"],
  ["config unmount", "not yet bound"],
  ["deployment cancel", "not yet bound"],
  ["deployment retry", "not yet bound"],
  ["domain check", "needs DNS lookups the CLI gathers"],
  ["domain rm", "not yet bound"],
  ["domain set", "not yet bound"],
  ["env branch", "not yet bound"],
  ["env copy", "not yet bound"],
  ["env default", "not yet bound"],
  ["env keep", "not yet bound"],
  ["env never-sync", "not yet bound"],
  ["env rm", "deletes an Environment, which the dashboard confirms by typing its name"],
  ["env shutdown", "not yet bound"],
  ["env sync", "not yet bound"],
  ["exec", "needs a terminal on a Server"],
  ["explain", "reads the local Setting catalog"],
  ["github disconnect", "asks a human; bound with the other always-approval commands"],
  ["logs", "streams from Servers"],
  ["org build-order", "not yet bound"],
  ["org ls", "the sidebar acts in the Organization you are in"],
  ["org rm", "deletes an Organization, which the dashboard confirms by typing its name"],
  ["org use", "the dashboard has one Organization switch"],
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
  ["service port-forward", "needs a local port"],
  ["service rename", "not yet bound"],
  ["service restart", "acts on running containers"],
  ["service start", "acts on running containers"],
  ["service stop", "acts on running containers"],
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
  ["volume move", "runs on Servers through Cloud workflows"],
  ["volume release", "runs on Servers through Cloud workflows"],
  ["volume rename", "not yet bound"],
  ["volume runs", "not yet bound"],
  ["volume set", "not yet bound"],
  ["volume sync", "runs on Servers through Cloud workflows"],
]);

/** `--accept-volume-loss` and `SERVICE` as tool input keys: `accept_volume_loss`, `service`. */
export const argKey = (name: string) => name.replace(/^--/, "").replaceAll("-", "_").toLowerCase();

export const toolName = (command: string) => command.replaceAll(" ", "_").replaceAll("-", "_");

export type JsonProperty =
  | { type: "string"; description?: string }
  | { type: "integer"; description?: string }
  | { type: "boolean"; description?: string }
  | { type: "array"; items: { type: "string" }; description?: string };
type JsonSchema = { type: "object"; properties: Record<string, JsonProperty>; required: string[]; additionalProperties: false };

const property = (arg: CatalogArg): JsonProperty => {
  const description = arg.help === undefined ? {} : { description: arg.help };
  if (arg.type === "boolean") return { type: "boolean", ...description };
  if (arg.type === "integer") return { type: "integer", ...description };
  return arg.multiple ? { type: "array", items: { type: "string" }, ...description } : { type: "string", ...description };
};

/** A command's tool input: the catalog's wording of each argument its binding reads, and what it adds, required. */
export function inputSchema(command: CatalogCommand, binding: AgentBinding): JsonSchema {
  const args = command.args.filter((arg) => binding.input.includes(argKey(arg.name)));
  const added = binding.added ?? {};
  return {
    type: "object",
    properties: { ...Object.fromEntries(args.map((arg) => [argKey(arg.name), property(arg)])), ...added },
    required: [...args.filter((arg) => arg.required).map((arg) => argKey(arg.name)), ...Object.keys(added)],
    additionalProperties: false,
  };
}

/** Every bound Cloud command with its catalog entry: the sidebar's tool set. */
export const AGENT_COMMANDS = COMMANDS.flatMap((command) => {
  const binding = BINDINGS.get(command.command);
  return command.surface === "cloud" && binding !== undefined ? [{ command, binding }] : [];
});
