import "@tanstack/react-start/server-only";
import { type AdapterYieldChunk, type DefaultMessageMetadataByModality, EventType, type ModelMessage, type TextOptions } from "@tanstack/ai";
import { BaseTextAdapter, type StructuredOutputResult } from "@tanstack/ai/adapters";
import { Option, Schema } from "effect";
import type { JsonObject } from "#/db/tables";
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
  refusal: Schema.Struct({ code: Schema.String, message: Schema.String, details: Schema.Unknown }),
}));
const Cancelled = Schema.fromJsonString(Schema.Struct({ ok: Schema.Literal(false), cancelled: Schema.Literal(true) }));
const Listed = Schema.fromJsonString(Schema.Struct({
  ok: Schema.Literal(true),
  value: Schema.Struct({ view: Schema.Literal("services"), services:Schema.Array(Schema.Struct({ name: Schema.String })) }),
}));
const CallScope = Schema.fromJsonString(Schema.Struct({
  project: Schema.optional(Schema.String), env: Schema.optional(Schema.String),
}));

type Scope = { readonly project?: string | undefined; readonly env?: string | undefined };

/** `scope` as tool input, naming only what it names. */
const scopeInput = (scope: Scope): JsonObject => {
  const input: JsonObject = {};
  if (scope.project !== undefined) input["project"] = scope.project;
  if (scope.env !== undefined) input["env"] = scope.env;
  return input;
};

/**
 * The retry accepting the Volume loss of the nearest `action` refused in `scope`, in that refusal's own Project and
 * Environment. A later result of that action, or any denial or cancellation, leaves nothing to accept.
 */
const lossRetry = (messages: ReadonlyArray<ModelMessage>, asked: number, action: "publish" | "deploy", scope: Scope): ScriptedTurn => {
  const calls = new Map(messages.slice(0, asked).flatMap((message) => message.role === "assistant" ? message.toolCalls ?? [] : [])
    .map((call) => [call.id, call]));
  for (let index = asked - 1; index >= 0; index--) {
    const result = messages[index];
    if (result?.role !== "tool") continue;
    if (Option.isSome(Schema.decodeUnknownOption(Cancelled)(text(result)))) break;
    const refused = Schema.decodeUnknownOption(Refused)(text(result));
    if (Option.isSome(refused) && refused.value.refusal.code === "approval_denied") break;
    const call = result.toolCallId === undefined ? undefined : calls.get(result.toolCallId);
    if (call?.function.name !== action) continue;
    const original = Option.getOrUndefined(Schema.decodeUnknownOption(CallScope)(call.function.arguments));
    if (original === undefined) break;
    if ((scope.project !== undefined && scope.project !== original.project) || (scope.env !== undefined && scope.env !== original.env)) continue;
    const loss = Option.isSome(refused) ? volumeLoss(refused.value.refusal) : null;
    if (loss === null || loss.accept.length === 0 || loss.accept.some((name) => !loss.volumes.some((volume) => volume.name === name))) break;
    const input = scopeInput(original);
    input["accept_volume_loss"] = [...loss.accept];
    input[action === "publish" ? "version" : "expect_version"] = loss.version;
    return { calls: [{ tool: action, input }] };
  }
  return { text: "No matching Volume-loss refusal to accept. Review that action in its original Project and Environment first." };
};

const text = ({ content }: ModelMessage) =>
  Array.isArray(content) ? content.flatMap((part) => (part.type === "text" ? [part.content] : [])).join("") : content ?? "";

/**
 * `PLOYZ_AGENT_STUB=1`'s model. It reads the member's latest message and the tool results since: "list services" lists
 * them by name, "list services and deploy" calls both at once, "set <path>=<value>" stages that Setting, "remove <service>"
 * and "drop <volume>" stage their removal, "publish" publishes (at `--version "<version>"` when named), and "deploy" deploys the
 * Environment, each in `--project` and `--env` when named. After a Publish or Deploy refuses Volume loss, an explicit request to accept it
 * retries that action in its original scope, accepting exactly what the Store named. A denial is quoted, never retried.
 */
export function stubScript(messages: ReadonlyArray<ModelMessage>): ScriptedTurn {
  const asked = messages.map((message) => message.role).lastIndexOf("user");
  const raw = asked < 0 ? "" : text(messages[asked] ?? { role: "user", content: "" });
  const said = raw.toLowerCase();
  const last = messages.slice(asked + 1).filter((message) => message.role === "tool").map(text).at(-1);
  if (last !== undefined) {
    if (Option.isSome(Schema.decodeUnknownOption(Cancelled)(last))) return { text: "The approval was cancelled, so nothing was deployed." };
    const listed = Schema.decodeUnknownOption(Listed)(last);
    if (Option.isSome(listed)) return { text: `Services: ${listed.value.value.services.map(({ name }) => name).join(", ")}.` };
    const refused = Schema.decodeUnknownOption(Refused)(last);
    if (Option.isNone(refused)) return { text: "Done." };
    const { code, message } = refused.value.refusal;
    return { text: code === "approval_denied" ? `I won't retry that. ${message}` : `The Store refused: ${message}` };
  }

  const assigned = /\bset ([a-z0-9._-]+=\S+)/.exec(said)?.[1];
  if (assigned !== undefined) return { calls: [{ tool: "set", input: { assignment: [assigned] } }] };
  const removed = /remove ([a-z0-9-]+)/.exec(said)?.[1];
  if (removed !== undefined) return { calls: [{ tool: "service_rm", input: { service: removed } }] };
  const dropped = /drop ([a-z0-9-]+)/.exec(said)?.[1];
  if (dropped !== undefined) return { calls: [{ tool: "volume_rm", input: { volume: dropped } }] };
  if (said.includes("list services and deploy")) return { calls: [{ tool: "service_ls", input: {} }, { tool: "deploy", input: {} }] };
  if (said.includes("list services")) return { calls: [{ tool: "service_ls", input: {} }] };
  const action = /\b(publish|deploy)\b/.exec(said)?.[1];
  const scope: Scope = { project: /--project ([a-z0-9-]+)/.exec(said)?.[1], env: /--env ([a-z0-9-]+)/.exec(said)?.[1] };
  if (said.includes("accepting the volume loss")) {
    return action === "publish" || action === "deploy"
      ? lossRetry(messages, asked, action, scope)
      : { text: "Name Publish or Deploy when accepting the Volume loss." };
  }
  if (action === "publish" || action === "deploy") {
    const input = scopeInput(scope);
    const version = action === "publish" ? /--version "([^"]*)"/.exec(raw)?.[1] : undefined;
    if (version !== undefined) input["version"] = version;
    return { calls: [{ tool: action, input }] };
  }
  return { text: "I can list services, remove one, or deploy." };
}

/** What an Organization's sidebar answers while Cloud has no model configured. */
export const notSetUpScript = (): ScriptedTurn => ({
  text: "The Ployz agent is not set up on this Cloud yet: it needs an Anthropic API key.",
});
