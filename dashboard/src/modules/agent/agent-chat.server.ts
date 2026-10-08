import "@tanstack/react-start/server-only";
import { createHash } from "node:crypto";
import type { ConfigCommand } from "@ployz/sdk";
import {
  type AnyTextAdapter,
  chat,
  type chatParamsFromRequest,
  defineChatMiddleware,
  EventType,
  type Interrupt,
  type ModelMessage,
  modelMessagesToUIMessages,
  type StreamChunk,
  toolDefinition,
  uiMessagesToWire,
} from "@tanstack/ai";
import { createAnthropicChat } from "@tanstack/ai-anthropic";
import { withPersistence } from "@tanstack/ai-persistence";
import { Config, Effect, Option, Redacted, Schema } from "effect";
import { approvalInterrupt, type ToolOutcome } from "#/modules/agent/agent";
import { AGENT_COMMANDS, type AgentBinding, inputSchema, toolName } from "#/modules/agent/agent-tools";
import { type AgentScope, agentPersistence, claimResume, releaseResume } from "#/modules/agent/persistence.server";
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
const DeniedApproval = Schema.Struct({ approval: Schema.Struct({ reason: Schema.NullOr(Schema.String) }) });

/** A denial as the agent hears it: what was denied and the human's reason, without the approval's ID to echo. */
const denialForAgent = (command: ConfigCommand, refusal: StoreRefusal): StoreRefusal => {
  if (refusal.code !== "approval_denied") return refusal;
  const reason = Option.match(Schema.decodeUnknownOption(DeniedApproval)(refusal.details), { onNone: () => null, onSome: ({ approval }) => approval.reason });
  const verb = command.command === "publish" ? "publish" : "deploy";
  return { code: refusal.code, message: `A human denied this ${verb}${reason === null ? "." : `: ${reason}`}`, details: { approval: { reason } } };
};

/** A UUID that only `approvalId` produces, so a second Deploy under one approval replays the first instead of running. */
const approvalUuid = (approvalId: string) => {
  const hex = createHash("sha256").update(`ployz.agent.deploy:${approvalId}`).digest("hex");
  const variant = ((Number.parseInt(hex[16] ?? "0", 16) & 0x3) | 0x8).toString(16);
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-8${hex.slice(13, 16)}-${variant}${hex.slice(17, 20)}-${hex.slice(20, 32)}`;
};

/**
 * `command` pinned to what one approval allows exactly once: a Deploy keyed by the approval, and a Publish of exactly the
 * approved version, which the Store refuses as stale once that Publish committed.
 */
const onceFor = (command: ConfigCommand, approvalId: string, digest: string): ConfigCommand => {
  if (command.command === "admit") return { ...command, id: approvalUuid(approvalId) };
  if (command.command === "publish") return { ...command, version: digest.slice(0, digest.indexOf(":")) };
  return command;
};

/**
 * One Publish or Deploy as `caller`, retried with `approvalId` once a human answered. When the plan destroys something and
 * the Organization asks first, it records the pending approval and answers with it instead of an outcome.
 */
const runGated = Effect.fn("Agent.runGated")(function* (caller: Caller, asked: ConfigCommand, approvalId: string | null) {
  const trusted = yield* trustedApproval(caller.organization.id, approvalId);
  if (!trusted.ok) return { outcome: { ok: false, refusal: denialForAgent(asked, trusted.refusal) } } satisfies Gated;
  const command = typeof trusted.approval === "object" && approvalId !== null ? onceFor(asked, approvalId, trusted.approval.approved) : asked;
  const result = yield* callStore(caller.organization.id, caller.userId, { operation: "write", command }, AGENT, trusted.approval);
  if (result.ok && trusted.approval === "required") return { outcome: { ...result, nothing_destroyed: true } } satisfies Gated;
  if (result.ok || result.refusal.code !== "approval_required") return { outcome: result } satisfies Gated;
  const request: StoreRefusal = yield* requestApproval(caller, command, result.refusal);
  const recorded = Schema.decodeUnknownOption(ApprovalAsked)(request.details);
  return (Option.isNone(recorded)
    ? { outcome: { ok: false, refusal: request } }
    : { asking: { approvalId: recorded.value.approval_id, message: request.message } }) satisfies Gated;
});

type AgentServices =
  | Effect.Services<ReturnType<typeof runGated>>
  | Effect.Services<ReturnType<typeof agentPersistence>>
  | Effect.Services<ReturnType<typeof claimResume>>;

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

/**
 * What a request adds to the stored thread: the member's new message, or nothing on a resumption. The client posts its
 * whole transcript, and its tool results carry no ids the store could match, so taking more would append them again.
 */
const added = (request: ChatRequest) => {
  const last = request.messages.at(-1);
  return request.resume === undefined && last?.role === "user" ? [last] : [];
};

type Persistence = Effect.Success<ReturnType<typeof agentPersistence>>;

/**
 * The turn another run already took for this answer, replayed instead of run again: the thread as it now stands, and
 * the approvals that turn left waiting.
 */
async function* answered(persistence: Persistence, request: ChatRequest): AsyncGenerator<StreamChunk> {
  const { threadId, runId } = request;
  yield { type: EventType.RUN_STARTED, threadId, runId, timestamp: Date.now() };
  const stored = await persistence.stores.messages.loadThread(threadId);
  const withIds = stored.map((message, index) => ({ ...message, id: message.id || `snapshot_${runId}_${index}` }));
  yield {
    type: EventType.MESSAGES_SNAPSHOT,
    timestamp: Date.now(),
    messages: uiMessagesToWire(modelMessagesToUIMessages(withIds), { includeSnapshotStructuredOutput: true, includeActivity: true }),
  };
  const waiting = await persistence.stores.interrupts.listPending(threadId);
  yield waiting.length === 0
    ? { type: EventType.RUN_FINISHED, threadId, runId, finishReason: "stop", timestamp: Date.now() }
    // SAFETY: the interrupt store keeps each interrupt exactly as the run that raised it published it.
    : { type: EventType.RUN_FINISHED, threadId, runId, outcome: { type: "interrupt", interrupts: waiting.map((record) => record.payload as Interrupt) }, timestamp: Date.now() };
}

const CLAIM_POLL_MS = 250;

/**
 * One answer runs at most once however many tabs or clients send it: the run that claims the interrupts resumes the
 * turn, and every other one waits for that turn to end and replays it.
 */
async function* resumeOnce(run: Run, persistence: Persistence, scope: AgentScope, request: ChatRequest & { resume: ReadonlyArray<{ interruptId: string }> }, resumed: () => AsyncIterable<StreamChunk>): AsyncGenerator<StreamChunk> {
  const ids = request.resume.map((entry) => entry.interruptId);
  for (;;) {
    const claim = await run(claimResume(scope, request.runId, ids));
    if (claim === "settled") return yield* answered(persistence, request);
    if (claim !== "busy") break;
    if (request.abortController?.signal.aborted === true) return;
    await new Promise((resolve) => setTimeout(resolve, CLAIM_POLL_MS));
  }
  try {
    yield* resumed();
  } finally {
    await run(releaseResume(scope, request.runId));
  }
}

/** One sidebar turn, or the resumption of one, as `caller` in their thread: the event stream the client renders. */
export const agentChat = Effect.fn("Agent.chat")(function* (caller: Caller, request: ChatRequest) {
  const run: Run = Effect.runPromiseWith(yield* Effect.context<AgentServices>());
  const scope: AgentScope = { organizationId: caller.organization.id, userId: caller.userId };
  const persistence = yield* agentPersistence(scope);
  const base = {
    adapter: yield* agentAdapter,
    messages: added(request),
    threadId: request.threadId,
    runId: request.runId,
    tools: agentTools(caller, run),
    systemPrompts: [systemPrompt(caller)],
    middleware: [withPersistence(persistence), approvalGate(caller, run)],
    interrupts: [approvalInterrupt],
  };
  const options: typeof base & { resume?: ChatRequest["resume"]; abortController?: AbortController } = base;
  if (request.abortController !== undefined) options.abortController = request.abortController;
  const { resume } = request;
  if (resume === undefined) return chat(options);
  options.resume = resume;
  return resumeOnce(run, persistence, scope, { ...request, resume }, () => chat(options));
});
