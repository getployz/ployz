import "@tanstack/react-start/server-only";
import type { ConfigCommand } from "@ployz/sdk";
import { type AnyTextAdapter, chat, type chatParamsFromRequest, defineChatMiddleware, type ModelMessage, toolDefinition } from "@tanstack/ai";
import { createAnthropicChat } from "@tanstack/ai-anthropic";
import { withPersistence } from "@tanstack/ai-persistence";
import { Config, Effect, Option, Redacted, Schema } from "effect";
import { approvalInterrupt, type ToolOutcome } from "#/modules/agent/agent";
import { AGENT_COMMANDS, type AgentBinding, inputSchema, toolName } from "#/modules/agent/agent-tools";
import { agentPersistence } from "#/modules/agent/persistence.server";
import { notSetUpScript, ScriptedAdapter, stubScript } from "#/modules/agent/scripted-adapter.server";
import { requestApproval, trustedApproval } from "#/modules/approvals/approvals.server";
import { callStore } from "#/modules/config-store/config-store.server";
import type { StoreCall, StoreRefusal } from "#/modules/config-store/store.contract";
import type { Caller } from "#/modules/identity/actor";
import { projectJsonValue } from "#/lib/json";

const AGENT = { source: "agent" } as const;
const GATED = new Map(AGENT_COMMANDS.flatMap(({ command, binding }) => binding.kind === "gated" ? [[toolName(command.command), binding]] : []));

type Gated = { outcome: ToolOutcome } | { asking: { approvalId: string; message: string } };
type Run = <A, E>(effect: Effect.Effect<A, E, AgentServices>) => Promise<A>;

const invalid = (message: string): ToolOutcome => ({ ok: false, refusal: { code: "invalid_argument", message, details: null } });

const storeCall = (binding: AgentBinding, input: ReturnType<typeof projectJsonValue>): StoreCall => {
  const raw = input ?? null;
  return binding.kind === "read" ? { operation: "read", query: binding.query(raw) } : { operation: "write", command: binding.command(raw) };
};

const ApprovalAsked = Schema.Struct({ approval_id: Schema.String });

/**
 * One Publish or Deploy as `caller`, retried with `approvalId` once a human answered. When the plan destroys something and
 * the Organization asks first, it records the pending approval and answers with it instead of an outcome.
 */
const runGated = Effect.fn("Agent.runGated")(function* (caller: Caller, command: ConfigCommand, approvalId: string | null) {
  const trusted = yield* trustedApproval(caller.organization.id, approvalId);
  if (!trusted.ok) return { outcome: { ok: false, refusal: trusted.refusal } } satisfies Gated;
  const result = yield* callStore(caller.organization.id, caller.userId, { operation: "write", command }, AGENT, trusted.approval);
  if (result.ok || result.refusal.code !== "approval_required") return { outcome: result } satisfies Gated;
  const asked: StoreRefusal = yield* requestApproval(caller, command, result.refusal);
  const recorded = Schema.decodeUnknownOption(ApprovalAsked)(asked.details);
  return (Option.isNone(recorded)
    ? { outcome: { ok: false, refusal: asked } }
    : { asking: { approvalId: recorded.value.approval_id, message: asked.message } }) satisfies Gated;
});

type AgentServices =
  | Effect.Services<ReturnType<typeof runGated>>
  | Effect.Services<ReturnType<typeof agentPersistence>>;

const decodeArguments = Schema.decodeUnknownOption(Schema.fromJsonString(Schema.Unknown));
const toolInput = (args: string) => projectJsonValue(Option.getOrElse(decodeArguments(args.trim() === "" ? "{}" : args), () => null));

/** The latest assistant turn's tool calls that have no result yet: what the tool phase is about to run. */
const pendingCalls = (messages: ReadonlyArray<ModelMessage>) => {
  const turn = messages.map((message) => message.role === "assistant" && (message.toolCalls?.length ?? 0) > 0).lastIndexOf(true);
  const answered = new Set(messages.slice(turn + 1).flatMap((message) => message.role === "tool" && message.toolCallId !== undefined ? [message.toolCallId] : []));
  return (messages[turn]?.toolCalls ?? []).filter((call) => !answered.has(call.id));
};

/**
 * Decides every gated call before the tool phase runs it, so a Publish or Deploy reaches the Store once per human answer.
 * A plan that needs a human raises `ployz.approval` keyed by the tool call; the resumed run decides again with the
 * approval the human answered, and every other outcome replaces the call's execution.
 */
const approvalGate = (caller: Caller, run: Run) => {
  const outcomes = new Map<string, ToolOutcome>();
  const answers = new Map<string, string | null>();
  const decide = (call: { id: string; function: { name: string; arguments: string } }, binding: AgentBinding) =>
    Effect.gen(function* (): Effect.fn.Return<Gated, Effect.Error<ReturnType<typeof runGated>>, AgentServices> {
      if (answers.has(call.id) && answers.get(call.id) === null) return { outcome: { ok: false, cancelled: true } };
      const command = yield* Effect.try(() => storeCall(binding, toolInput(call.function.arguments))).pipe(Effect.option);
      if (Option.isNone(command) || command.value.operation !== "write") {
        return { outcome: invalid(`The ${call.function.name} input doesn't match its schema.`) };
      }
      return yield* runGated(caller, command.value.command, answers.get(call.id) ?? null);
    });

  return defineChatMiddleware<unknown, readonly [], readonly [], typeof approvalInterrupt>({
    name: "ployz-approval-gate",
    onInterruptResolution: (_ctx, resolutions) => {
      for (const resolution of resolutions.for(approvalInterrupt)) {
        answers.set(resolution.request.key, resolution.status === "resolved" ? resolution.request.payload?.approvalId ?? null : null);
      }
      return { toolResume: "continue" };
    },
    onInterruptBoundary: async (ctx) => {
      if (ctx.phase !== "beforeTools") return undefined;
      const calls = pendingCalls(ctx.messages);
      const gated = calls.flatMap((call) => {
        const binding = GATED.get(call.function.name);
        return binding === undefined ? [] : [{ call, binding }];
      });
      if (calls.length > 1) {
        for (const { call } of gated) {
          outcomes.set(call.id, invalid(`Call ${call.function.name} alone, in its own turn, so a human can review exactly that plan.`));
        }
        return undefined;
      }
      const [only] = gated;
      if (only === undefined) return undefined;
      const decided = await run(decide(only.call, only.binding));
      if ("outcome" in decided) {
        outcomes.set(only.call.id, decided.outcome);
        return undefined;
      }
      const { approvalId, message } = decided.asking;
      return { interrupts: [approvalInterrupt.interrupt({ key: only.call.id, reason: "approval_required", message, payload: { approvalId } })] };
    },
    onBeforeToolCall: (_ctx, { toolCallId }) => {
      const outcome = outcomes.get(toolCallId);
      return outcome === undefined ? undefined : { type: "skip", result: outcome };
    },
  });
};

/** Every bound Cloud command as a tool. Reads and staged writes go straight to the Store; the gate answers gated ones. */
const agentTools = (caller: Caller, run: Run) => AGENT_COMMANDS.map(({ command, binding }) =>
  toolDefinition({ name: toolName(command.command), description: command.about, inputSchema: inputSchema(command, binding) })
    .server((args) => {
      if (binding.kind === "gated") throw new Error(`${command.command} ran outside the approval gate.`);
      return run(callStore(caller.organization.id, caller.userId, storeCall(binding, projectJsonValue(args)), AGENT));
    }));

const systemPrompt = (caller: Caller) => `You are the Ployz agent in the sidebar of Ployz Cloud. You act in the Organization "${caller.organization.slug}" as the member who is talking to you, through the same Config Store the ployz CLI uses. Never act in another Organization.

Read before you write: list or inspect what a command touches before you change it. Staged writes (service rm, volume rm, set and the like) only change the next Version; publish and deploy make them real.

Every tool answers { ok: true, value } or { ok: false, refusal: { code, message, details } }. When the Store refuses, tell the member its message verbatim before anything else.

A Deploy that permanently deletes a Volume's data refuses with confirmation_required, naming the Version and the Volumes in details. Retry it only when the member asked for exactly that loss, passing accept_volume_loss with those Volumes and expect_version with that Version.

Publish and deploy may wait for a human to approve the plan. Call either one alone, never alongside another tool. When a human denies an approval (approval_denied), quote the reason they gave and do not retry that action or work around it. When an approval is cancelled, say nothing was published or deployed.`;

/** The model the sidebar talks to: the scripted stub under `PLOYZ_AGENT_STUB=1`, Claude when Cloud has an Anthropic key. */
const agentAdapter = Effect.gen(function* () {
  if ((yield* Config.option(Config.string("PLOYZ_AGENT_STUB"))).pipe(Option.contains("1"))) return new ScriptedAdapter(stubScript);
  const key = yield* Config.option(Config.redacted("ANTHROPIC_API_KEY"));
  return Option.match(key, {
    onNone: (): AnyTextAdapter => new ScriptedAdapter(notSetUpScript),
    onSome: (secret): AnyTextAdapter => createAnthropicChat("claude-sonnet-5-5", Redacted.value(secret)),
  });
}).pipe(Effect.orDie);

type ChatRequest = Pick<Awaited<ReturnType<typeof chatParamsFromRequest>>, "messages" | "threadId" | "runId" | "resume"> & {
  readonly abortController?: AbortController;
};

/** One sidebar turn, or the resumption of one, as `caller` in their thread: the event stream the client renders. */
export const agentChat = Effect.fn("Agent.chat")(function* (caller: Caller, request: ChatRequest) {
  const run: Run = Effect.runPromiseWith(yield* Effect.context<AgentServices>());
  const persistence = yield* agentPersistence({ organizationId: caller.organization.id, userId: caller.userId });
  const base = {
    adapter: yield* agentAdapter,
    messages: request.messages,
    threadId: request.threadId,
    runId: request.runId,
    tools: agentTools(caller, run),
    systemPrompts: [systemPrompt(caller)],
    middleware: [withPersistence(persistence), approvalGate(caller, run)],
    interrupts: [approvalInterrupt],
  };
  const options: typeof base & { resume?: ChatRequest["resume"]; abortController?: AbortController } = base;
  if (request.resume !== undefined) options.resume = request.resume;
  if (request.abortController !== undefined) options.abortController = request.abortController;
  return chat(options);
});
