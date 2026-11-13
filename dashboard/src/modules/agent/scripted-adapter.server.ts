import "@tanstack/react-start/server-only";
import { type AdapterYieldChunk, type DefaultMessageMetadataByModality, EventType, type ModelMessage, type TextOptions } from "@tanstack/ai";
import { BaseTextAdapter, type StructuredOutputResult } from "@tanstack/ai/adapters";
import { Option, Schema } from "effect";
import type { JsonObject } from "#/db/tables";
import { isPageContext, latestPageContext, readPageContext } from "#/modules/agent/page-context";
import { volumeLoss } from "#/modules/config-store/store-volumes";

/** One model turn: words for the member, or the tool calls it makes at once. */
export type ScriptedTurn = { text: string } | { calls: ReadonlyArray<{ tool: string; input: JsonObject }> };

/** A model that answers from the conversation by rule, for tests and for an unconfigured Cloud. */
export class ScriptedAdapter extends BaseTextAdapter<"scripted", Record<string, never>, readonly ["text"], DefaultMessageMetadataByModality> {
  readonly name = "scripted";

  constructor(private readonly script: (messages: ReadonlyArray<ModelMessage>) => ScriptedTurn) {
    super(undefined, "scripted");
  }

  async *chatStream(options: TextOptions<Record<string, never>>): AsyncIterable<AdapterYieldChunk> {
    const { runId = crypto.randomUUID(), threadId = crypto.randomUUID() } = options;
    const model = this.model;
    const timestamp = Date.now();
    yield { type: EventType.RUN_STARTED, runId, threadId, model, timestamp };
    const turn = this.script(options.messages);
    if ("text" in turn) {
      const messageId = crypto.randomUUID();
      yield { type: EventType.TEXT_MESSAGE_START, messageId, model, timestamp, role: "assistant" };
      yield { type: EventType.TEXT_MESSAGE_CONTENT, messageId, model, timestamp, delta: turn.text, content: turn.text };
      yield { type: EventType.TEXT_MESSAGE_END, messageId, model, timestamp };
      yield { type: EventType.RUN_FINISHED, runId, threadId, model, timestamp, finishReason: "stop" };
      return;
    }
    for (const [index, call] of turn.calls.entries()) {
      const toolCallId = `call_${crypto.randomUUID()}`;
      const args = JSON.stringify(call.input);
      const named = { toolCallId, toolCallName: call.tool, toolName: call.tool, model, timestamp };
      yield { type: EventType.TOOL_CALL_START, ...named, index };
      yield { type: EventType.TOOL_CALL_ARGS, toolCallId, model, timestamp, delta: args, args };
      yield { type: EventType.TOOL_CALL_END, ...named, input: call.input };
    }
    yield { type: EventType.RUN_FINISHED, runId, threadId, model, timestamp, finishReason: "tool_calls" };
  }

  structuredOutput(): Promise<StructuredOutputResult<unknown>> {
    return Promise.reject(new Error("The scripted model writes no structured output."));
  }
}

const Refused = Schema.fromJsonString(Schema.Struct({
  ok: Schema.Literal(false),
  refusal: Schema.Struct({ code: Schema.String, message: Schema.String, details: Schema.optional(Schema.Unknown) }),
}));
const Cancelled = Schema.fromJsonString(Schema.Struct({ ok: Schema.Literal(false), cancelled: Schema.Literal(true) }));
const Listed = Schema.fromJsonString(Schema.Struct({
  ok: Schema.Literal(true),
  value: Schema.Struct({ projects: Schema.Array(Schema.Unknown), services: Schema.Array(Schema.Struct({ name: Schema.String })) }),
}));
const CallScope = Schema.fromJsonString(Schema.Struct({
  project: Schema.optional(Schema.String), environment: Schema.optional(Schema.String), skip_deploy: Schema.optional(Schema.Boolean),
}));

type Scope = { readonly project?: string | undefined; readonly environment?: string | undefined };

/** `scope` as tool input, naming only what it names. */
const scopeInput = (scope: Scope): JsonObject => {
  const input: JsonObject = {};
  if (scope.project !== undefined) input["project"] = scope.project;
  if (scope.environment !== undefined) input["environment"] = scope.environment;
  return input;
};

/** "save" is the deploy tool with skip_deploy; "deploy" is the deploy tool without it. */
type Action = "save" | "deploy";

const actionInput = (action: Action, scope: Scope): JsonObject => {
  const input = scopeInput(scope);
  if (action === "save") input["skip_deploy"] = true;
  return input;
};

/**
 * The retry accepting the Volume loss of the nearest `action` refused in `scope`, in that refusal's own Project and
 * Environment. A later result of that action, or any denial or cancellation, leaves nothing to accept.
 */
const lossRetry = (messages: ReadonlyArray<ModelMessage>, asked: number, action: Action, scope: Scope): ScriptedTurn => {
  const calls = new Map(messages.slice(0, asked).flatMap((message) => message.role === "assistant" ? message.toolCalls ?? [] : [])
    .map((call) => [call.id, call]));
  for (let index = asked - 1; index >= 0; index--) {
    const result = messages[index];
    if (result?.role !== "tool") continue;
    if (Option.isSome(Schema.decodeUnknownOption(Cancelled)(text(result)))) break;
    const refused = Schema.decodeUnknownOption(Refused)(text(result));
    if (Option.isSome(refused) && refused.value.refusal.code === "approval_denied") break;
    const call = result.toolCallId === undefined ? undefined : calls.get(result.toolCallId);
    if (call?.function.name !== "deploy") continue;
    const original = Option.getOrUndefined(Schema.decodeUnknownOption(CallScope)(call.function.arguments));
    if (original === undefined) break;
    if ((original.skip_deploy === true) !== (action === "save")) continue;
    if ((scope.project !== undefined && scope.project !== original.project) || (scope.environment !== undefined && scope.environment !== original.environment)) continue;
    const loss = Option.isSome(refused) ? volumeLoss({ details: null, ...refused.value.refusal }) : null;
    if (loss === null || loss.accept.length === 0 || loss.accept.some((name) => !loss.volumes.some((volume) => volume.name === name))) break;
    const input = actionInput(action, original);
    input["accept_volume_loss"] = [...loss.accept];
    input["version"] = loss.version;
    return { calls: [{ tool: "deploy", input }] };
  }
  return { text: "No matching Volume-loss refusal to accept. Review that action in its original Project and Environment first." };
};

const Opened = Schema.fromJsonString(Schema.Struct({ ok: Schema.Literal(true), value: Schema.Struct({ proposal_id: Schema.String }) }));
const Staged = Schema.fromJsonString(Schema.Struct({ ok: Schema.Literal(true), value: Schema.Struct({ step: Schema.Number }) }));
const Presented = Schema.fromJsonString(Schema.Struct({ ok: Schema.Literal(true), value: Schema.Struct({ steps: Schema.Number }) }));
const Followed = Schema.fromJsonString(Schema.Struct({
  ok: Schema.Literal(true),
  value: Schema.Struct({
    status: Schema.String,
    steps: Schema.Array(Schema.Struct({ index: Schema.Number, verb: Schema.String, service: Schema.String, state: Schema.String, reason: Schema.NullOr(Schema.String) })),
  }),
}));

/** "Approved. 1. stop web: done. 2. restart api: failed, machine-2 didn't accept the approval. 3. start web: not run." */
const standing = ({ status, steps }: typeof Followed.Type["value"]) => {
  const head = status.charAt(0).toUpperCase() + status.slice(1);
  const lines = steps.map(({ index, verb, service, state, reason }) => `${index}. ${verb} ${service}: ${state.replace("_", " ")}${reason === null ? "." : `, ${reason.replace(/\.$/, "")}.`}`);
  return [`${head}.`, ...lines].join(" ");
};

/**
 * "propose stop web, restart api, start web": open a proposal, stage each step in turn with `proposal` set to it, then
 * present it. Each turn reads how far the tool results since the request got. A refusal on the way is quoted.
 */
const proposing = (results: ReadonlyArray<string>, plan: string): ScriptedTurn => {
  const steps = plan.split(",").flatMap((part) => {
    const step = /\b(stop|start|restart) ([a-z0-9-]+)/.exec(part.trim());
    return step === null ? [] : [{ verb: step[1] ?? "", service: step[2] ?? "" }];
  });
  const last = results.at(-1);
  const refused = last === undefined ? Option.none() : Schema.decodeUnknownOption(Refused)(last);
  if (Option.isSome(refused)) return { text: `The proposal stopped: ${refused.value.refusal.message}` };
  const opened = results.flatMap((result) => Option.toArray(Schema.decodeUnknownOption(Opened)(result))).at(0);
  if (opened === undefined) return { calls: [{ tool: "proposal_create", input: {} }] };
  const proposal = opened.value.proposal_id;
  const staged = results.filter((result) => Option.isSome(Schema.decodeUnknownOption(Staged)(result))).length;
  const next = steps[staged];
  if (next !== undefined) return { calls: [{ tool: `service_${next.verb}`, input: { service: next.service, proposal } }] };
  if (results.some((result) => Option.isSome(Schema.decodeUnknownOption(Presented)(result)))) return { text: `Presented proposal ${proposal}. Say "follow ${proposal}" to see how far it got.` };
  return { calls: [{ tool: "proposal_present", input: { proposal } }] };
};

const text = ({ content }: ModelMessage) =>
  Array.isArray(content) ? content.flatMap((part) => (part.type === "text" ? [part.content] : [])).join("") : content ?? "";

/** What the member typed, without the page block Cloud put before it, so a page named "deploy" deploys nothing. */
const typed = ({ content }: ModelMessage) =>
  Array.isArray(content)
    ? content.flatMap((part) => (part.type === "text" && !isPageContext(part.content) ? [part.content] : [])).join("")
    : content ?? "";

/** The latest page block in plain words. */
const whereAbouts = (messages: ReadonlyArray<ModelMessage>): string => {
  const block = latestPageContext(messages);
  if (block === undefined) return "I don't know where you are.";
  const at = readPageContext(block);
  const place = [at.get("project"), at.get("environment")].filter((part) => part !== undefined).join(" / ");
  const named = [["Service", at.get("service")], ["Deployment", at.get("deployment")], ["Server", at.get("server")]]
    .flatMap(([noun, value]) => value === undefined ? [] : [`, ${noun} ${value}`]).join("");
  return `You are on ${at.get("page") ?? "a page"}${place ? ` in ${place}` : ""}${named}.`;
};

/**
 * `PLOYZ_AGENT_STUB=1`'s model. It reads the member's latest message and the tool results since: "list services" lists
 * them by name, "list services and deploy" calls both at once, "set <service>.<setting>=<value>" stages that Setting,
 * "remove <service>" and "drop <volume>" stage their removal, "save" saves without deploying (at `--version "<version>"`
 * when named), and "deploy" deploys the Environment, each in `--project` and `--environment` when named. "tree
 * <owner/repo>" lists a repository's files. After a Save or Deploy refuses Volume loss, an explicit request to accept it
 * retries that action in its original scope, accepting exactly what the Store named. A denial is quoted, never retried.
 * "Where am I" answers from the thread's latest page block. "stop", "start" or "restart <service>" runs that verb on it,
 * "propose stop web, restart api" stages those steps into one proposal and presents it, "follow <id>" reads where that
 * proposal stands, and "show a picture" answers with a Markdown image.
 */
export function stubScript(messages: ReadonlyArray<ModelMessage>): ScriptedTurn {
  const asked = messages.map((message) => message.role).lastIndexOf("user");
  const raw = asked < 0 ? "" : typed(messages[asked] ?? { role: "user", content: "" });
  const said = raw.toLowerCase();
  const results = messages.slice(asked + 1).filter((message) => message.role === "tool").map(text);
  const plan = /\bpropose (.+)/.exec(said)?.[1];
  if (plan !== undefined) return proposing(results, plan);
  const followed = /\bfollow ([0-9a-f-]{36})/.exec(said)?.[1];
  if (followed !== undefined && results.length === 0) return { calls: [{ tool: "proposal_status", input: { proposal: followed } }] };
  const last = results.at(-1);
  if (last !== undefined) {
    const stands = Schema.decodeUnknownOption(Followed)(last);
    if (Option.isSome(stands)) return { text: standing(stands.value.value) };
    if (Option.isSome(Schema.decodeUnknownOption(Cancelled)(last))) return { text: "The approval was cancelled, so nothing was deployed." };
    const listed = Schema.decodeUnknownOption(Listed)(last);
    if (Option.isSome(listed)) return { text: `Services: ${listed.value.value.services.map(({ name }) => name).join(", ")}.` };
    const refused = Schema.decodeUnknownOption(Refused)(last);
    if (Option.isNone(refused)) return { text: "Done." };
    const { code, message } = refused.value.refusal;
    return { text: code === "approval_denied" ? `I won't retry that. ${message}` : `The Store refused: ${message}` };
  }

  if (said.includes("where am i")) return { text: whereAbouts(messages) };
  const repository = /\btree ([a-z0-9._-]+\/[a-z0-9._-]+)/.exec(said)?.[1];
  if (repository !== undefined) return { calls: [{ tool: "github_ls", input: { repository } }] };
  const assigned = /\bset ([a-z0-9-]+)\.([a-z][a-z0-9]*)=(\S+)/i.exec(raw);
  if (assigned !== null) {
    const [, service = "", setting = "", value = ""] = assigned;
    return { calls: [{ tool: "service_update", input: { service: service.toLowerCase(), set: { [setting]: value } } }] };
  }
  const acted = /\b(stop|start|restart) ([a-z0-9-]+)/.exec(said);
  if (acted !== null) {
    const [, verb = "", service = ""] = acted;
    return { calls: [{ tool: `service_${verb}`, input: { service } }] };
  }
  if (said.includes("show a picture")) return { text: "Here it is: ![chart](https://example.com/chart.png?from=logs)" };
  const removed = /remove ([a-z0-9-]+)/.exec(said)?.[1];
  if (removed !== undefined) return { calls: [{ tool: "service_remove", input: { service: removed } }] };
  const dropped = /drop ([a-z0-9-]+)/.exec(said)?.[1];
  if (dropped !== undefined) return { calls: [{ tool: "volume_remove", input: { volume: dropped } }] };
  if (said.includes("list services and deploy")) return { calls: [{ tool: "environment_show", input: {} }, { tool: "deploy", input: {} }] };
  if (said.includes("list services")) return { calls: [{ tool: "environment_show", input: {} }] };
  const action = /\b(save|deploy)\b/.exec(said)?.[1];
  const scope: Scope = { project: /--project ([a-z0-9-]+)/.exec(said)?.[1], environment: /--environment ([a-z0-9-]+)/.exec(said)?.[1] };
  if (said.includes("accepting the volume loss")) {
    return action === "save" || action === "deploy"
      ? lossRetry(messages, asked, action, scope)
      : { text: "Name Save or Deploy when accepting the Volume loss." };
  }
  if (action === "save" || action === "deploy") {
    const input = actionInput(action, scope);
    const version = action === "save" ? /--version "([^"]*)"/.exec(raw)?.[1] : undefined;
    if (version !== undefined) input["version"] = version;
    return { calls: [{ tool: "deploy", input }] };
  }
  return { text: "I can list services, remove one, or deploy." };
}

/** What an Organization's sidebar answers while Cloud has no model configured. */
export const notSetUpScript = (): ScriptedTurn => ({
  text: "The Ployz agent is not set up on this Cloud yet: it needs an Anthropic API key.",
});
