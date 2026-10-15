import "@tanstack/react-start/server-only";
import { randomUUID } from "node:crypto";
import {
  type AnyTextAdapter,
  chat,
  type chatParamsFromRequest,
  defineChatMiddleware,
  EventType,
  isTerminalRunStatus,
  maxIterations,
  type ModelMessage,
  modelMessagesToUIMessages,
  type StreamChunk,
  toolDefinition,
  uiMessagesToWire,
} from "@tanstack/ai";
import { type AnthropicChatModelProviderOptionsByName, createAnthropicChat } from "@tanstack/ai-anthropic";
import { withPersistence } from "@tanstack/ai-persistence";
import { Config, Effect, Option, Redacted, Schema } from "effect";
import { approvalInterrupt, type ToolOutcome } from "#/modules/agent/agent";
import { AGENT_TOOLS, type AgentCaller, type AgentResult, runAgentCall } from "#/modules/agent/agent-runner.server";
import { agentTracing, aiObservability } from "#/modules/agent/agent-telemetry.server";
import { CATALOG, GATED } from "#/modules/agent/agent-tools";
import {
  type AgentScope,
  agentPersistence,
  CLAIM_LEASE_MS,
  claimResume,
  releaseResume,
  renewResume,
  startRun,
  Superseded,
} from "#/modules/agent/persistence.server";
import { FAILURES, providerFailure } from "#/modules/agent/provider-error";
import { latestPageContext, PageContext, type PagePlace, placeOfPage, renderPageContext, withPageContext } from "#/modules/agent/page-context";
import { notSetUpScript, ScriptedAdapter, stubScript } from "#/modules/agent/scripted-adapter.server";
import { PostHog } from "#/modules/analytics/posthog.server";
import { decideApproval } from "#/modules/approvals/approvals.server";
import { readStore } from "#/modules/config-store/config-store.server";
import type { Caller } from "#/modules/identity/actor";
import { projectJsonValue } from "#/lib/json";

type Gated = { outcome: ToolOutcome } | { asking: { approvalId: string; message: string } };
type Run = <A, E>(effect: Effect.Effect<A, E, AgentServices>) => Promise<A>;

const invalid = (message: string): ToolOutcome => ({ ok: false, refusal: { code: "invalid_argument", message, details: null } });

/** A gated call as the sidebar shows it. */
const shown = (result: AgentResult): Gated =>
  "asking" in result
    ? { asking: { approvalId: result.asking.approvalId, message: result.asking.refusal.message } }
    : { outcome: result.outcome };

const outcomeOf = (result: AgentResult): ToolOutcome => "asking" in result ? { ok: false, refusal: result.asking.refusal } : result.outcome;

type AgentServices =
  | Effect.Services<ReturnType<typeof runAgentCall>>
  | Effect.Services<ReturnType<typeof agentPersistence>>;

const decodeArguments = Schema.decodeUnknownOption(Schema.fromJsonString(Schema.Unknown));
const toolInput = (args: string) => projectJsonValue(Option.getOrElse(decodeArguments(args.trim() === "" ? "{}" : args), () => null)) ?? null;

const placeIn = (caller: AgentCaller, messages: ReadonlyArray<ModelMessage>) => {
  const organizationId = caller.organization.id;
  return placeOfPage(
    latestPageContext(messages),
    readStore(organizationId, { query: "projects" }).pipe(Effect.map(({ projects }) => projects.map(({ name }) => name))),
    (project) => readStore(organizationId, { query: "environments", project }).pipe(
      Effect.map((view) => ({ project: view.project.name, environments: view.environments })),
    ),
  );
};

type Here = { page: PagePlace };

/** The latest assistant turn's tool calls that have no result yet: what the tool phase is about to run. */
const pendingCalls = (messages: ReadonlyArray<ModelMessage>) => {
  const turn = messages.map((message) => message.role === "assistant" && (message.toolCalls?.length ?? 0) > 0).lastIndexOf(true);
  const answered = new Set(messages.slice(turn + 1).flatMap((message) => message.role === "tool" && message.toolCallId !== undefined ? [message.toolCallId] : []));
  return (messages[turn]?.toolCalls ?? []).filter((call) => !answered.has(call.id));
};

const approvalGate = (caller: AgentCaller, run: Run, here: Here) => {
  const outcomes = new Map<string, ToolOutcome>();
  const answers = new Map<string, { approvalId: string | null; cancelled: boolean }>();
  const decide = (call: { id: string; function: { name: string; arguments: string } }, turn: string) =>
    Effect.gen(function* (): Effect.fn.Return<Gated, Effect.Error<ReturnType<typeof runAgentCall>>, AgentServices> {
      const answer = answers.get(call.id);
      if (answer?.cancelled === true) {
        if (answer.approvalId !== null) {
          yield* decideApproval(caller, answer.approvalId, { reject: {} }).pipe(Effect.catchTag("NotFound", () => Effect.void));
        }
        return { outcome: { ok: false, cancelled: true } };
      }
      const input = toolInput(call.function.arguments);
      return shown(yield* runAgentCall(caller, { tool: call.function.name, input, key: `${turn}:${call.id}`, approvalId: answer?.approvalId ?? null, page: here.page }));
    });

  return defineChatMiddleware<unknown, readonly [], readonly [], typeof approvalInterrupt>({
    name: "ployz-approval-gate",
    onInterruptResolution: (_ctx, resolutions) => {
      for (const resolution of resolutions.for(approvalInterrupt)) {
        answers.set(resolution.request.key, { approvalId: resolution.request.payload?.approvalId ?? null, cancelled: resolution.status === "cancelled" });
      }
      return { toolResume: "continue" };
    },
    onInterruptBoundary: async (ctx) => {
      if (ctx.phase !== "beforeTools") return undefined;
      here.page = await run(placeIn(caller, ctx.messages));
      const calls = pendingCalls(ctx.messages);
      const gated = calls.filter((call) => GATED.has(call.function.name));
      if (calls.length > 1) {
        for (const call of gated) {
          outcomes.set(call.id, invalid(`Call ${call.function.name} alone, in its own turn, so a human can review exactly that plan.`));
        }
        return undefined;
      }
      const [only] = gated;
      if (only === undefined) return undefined;
      const decided = await run(decide(only, ctx.runId));
      if ("outcome" in decided) {
        outcomes.set(only.id, decided.outcome);
        return undefined;
      }
      const { approvalId, message } = decided.asking;
      return { interrupts: [approvalInterrupt.interrupt({ key: only.id, reason: "approval_required", message, payload: { approvalId } })] };
    },
    onBeforeToolCall: (_ctx, { toolCallId }) => {
      const outcome = outcomes.get(toolCallId);
      return outcome === undefined ? undefined : { type: "skip", result: outcome };
    },
  });
};

/** Every agent tool. A gated one runs in the approval gate, so its own handler never does. */
const agentTools = (caller: AgentCaller, run: Run, turn: string, here: Here) => AGENT_TOOLS.map((tool) =>
  toolDefinition({ name: tool.name, description: tool.description, inputSchema: tool.inputSchema })
    .server((args, context) => {
      if (GATED.has(tool.name)) throw new Error(`${tool.name} ran outside the approval gate.`);
      const key = `${turn}:${context?.toolCallId ?? randomUUID()}`;
      return run(runAgentCall(caller, { tool: tool.name, input: projectJsonValue(args) ?? null, key, approvalId: null, page: here.page }).pipe(Effect.map(outcomeOf)));
    }));

const systemPrompt = (caller: AgentCaller, model: string) => `${CATALOG.instructions}

You are ${model}, the Ployz agent in the sidebar of Ployz Cloud. You act in the Organization "${caller.organization.slug}" as the member who is talking to you. Never act in another Organization.

Your place is the Project and Environment named by the latest <dashboard-page …/> block, which opens a member message whenever they move. When they say "this service" or "here", use that block's. Never mention the block itself. With no block, the Organization's defaults apply.

A Deploy (skip_deploy included), removing an Environment or a Project, a server drain, cleanup, removal or forget, turning web traffic off on a server, and a Volume mirror, mirror removal, move or release may wait for a human to approve the plan. Call any of them alone, never alongside another tool. When a human denies an approval (approval_denied), quote the reason they gave and do not retry that action or work around it. When an approval is cancelled, say nothing changed. A Service restart, start or stop never waits, and neither does a server upgrade.`;

/**
 * Opens the member's new message with where they are in the dashboard, when that differs from the thread's last block.
 * It runs after withPersistence merged the stored thread, so the block is saved with the message: replays send the same
 * bytes and the prompt prefix stays cached.
 */
const pageMarker = (page: PageContext) => defineChatMiddleware({
  name: "ployz-page-context",
  onConfig: (ctx, config) => {
    if (ctx.phase !== "init") return undefined;
    const last = config.messages.at(-1);
    if (last?.role !== "user") return undefined;
    const earlier = config.messages.slice(0, -1);
    const block = renderPageContext(page);
    if (latestPageContext(earlier) === block) return undefined;
    return { messages: [...earlier, withPageContext(last, block)] };
  },
});

/**
 * Model turns one member message may take. Deploying a GitHub repository takes 14 before its save waits on a human,
 * reading one file per turn as Haiku does; a model looping on one tool spends no more than this.
 */
export const STEP_LIMIT = 25;
export const STEP_LIMIT_NOTE = `I stopped after ${STEP_LIMIT} steps, my limit for one message. Say "continue" and I'll pick up where I left off.`;

/**
 * Ends a turn the step limit cut off after its tool calls ran, before the model read their results, with a note the
 * member sees and the thread keeps. The engine streams nothing after that tool phase, so `noted` appends the note to the
 * stream, and the middleware saves it after withPersistence saved the turn: a reload shows it, and the member's
 * "continue" follows an assistant message.
 */
const stepLimit = ({ stores }: Persistence) => {
  let exhausted = false;
  const messageId = randomUUID();
  const middleware = defineChatMiddleware({
    name: "ployz-step-limit",
    onShouldContinue: (_ctx, { iterationCount, finishReason }) => {
      exhausted = iterationCount >= STEP_LIMIT && finishReason === "tool_calls";
      return undefined;
    },
    onFinish: async (ctx) => {
      if (!exhausted) return;
      const thread = await stores.messages.loadThread(ctx.threadId);
      await stores.messages.saveThread(ctx.threadId, [...thread, { id: messageId, role: "assistant", content: STEP_LIMIT_NOTE }]);
    },
  });
  async function* noted(stream: AsyncIterable<StreamChunk>): AsyncGenerator<StreamChunk> {
    yield* stream;
    if (!exhausted) return;
    const timestamp = Date.now();
    yield { type: EventType.TEXT_MESSAGE_START, messageId, role: "assistant", timestamp };
    yield { type: EventType.TEXT_MESSAGE_CONTENT, messageId, delta: STEP_LIMIT_NOTE, timestamp };
    yield { type: EventType.TEXT_MESSAGE_END, messageId, timestamp };
  }
  return { middleware, noted };
};

/**
 * The stream with each provider failure retold in the sidebar's words, coded by what it means for the member. The
 * provider's own message, JSON body and request id go to Cloud's log, not to the member.
 */
async function* plainFailures(run: Run, stream: AsyncIterable<StreamChunk>): AsyncGenerator<StreamChunk> {
  for await (const chunk of stream) {
    const failure = chunk.type === EventType.RUN_ERROR ? providerFailure(chunk) : undefined;
    if (chunk.type !== EventType.RUN_ERROR || failure === undefined) {
      yield chunk;
      continue;
    }
    await run(Effect.logError("Claude failed an agent turn.", { failure, code: chunk.code, message: chunk.message }));
    const { rawEvent: _rawEvent, error: _error, ...rest } = chunk;
    yield { ...rest, message: FAILURES[failure], code: failure };
  }
}

const CLAUDE = "claude-sonnet-5-5";
type AgentModel = { adapter: AnyTextAdapter; modelOptions?: AnthropicChatModelProviderOptionsByName[typeof CLAUDE] };
const scripted = (script: ConstructorParameters<typeof ScriptedAdapter>[0]): AgentModel => ({ adapter: new ScriptedAdapter(script) });

/**
 * The model the sidebar talks to: the scripted stub under `PLOYZ_AGENT_STUB=1`, Claude when Cloud has an Anthropic key.
 * Claude caches the prompt up to its last block, so a turn rereads the thread before it from cache. It thinks as much as
 * each turn needs and sends a summary of that thinking, which the sidebar shows; left at its default, Claude sends none.
 */
const agentModel = Effect.gen(function* () {
  if ((yield* Config.option(Config.string("PLOYZ_AGENT_STUB"))).pipe(Option.contains("1"))) return scripted(stubScript);
  const key = yield* Config.option(Config.redacted("ANTHROPIC_API_KEY"));
  return Option.match(key, {
    onNone: () => scripted(notSetUpScript),
    onSome: (secret): AgentModel => ({
      adapter: createAnthropicChat(CLAUDE, Redacted.value(secret)),
      modelOptions: { cache_control: { type: "ephemeral" }, thinking: { type: "adaptive", display: "summarized" } },
    }),
  });
}).pipe(Effect.orDie);

type ChatParams = Awaited<ReturnType<typeof chatParamsFromRequest>>;
type ChatRequest = Pick<ChatParams, "messages" | "threadId" | "runId" | "resume"> & Partial<Pick<ChatParams, "forwardedProps">> & {
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

const StoredInterrupt = Schema.Struct({
  subagentRunId: Schema.optionalKey(Schema.String),
  id: Schema.String,
  reason: Schema.String,
  message: Schema.optionalKey(Schema.String),
  toolCallId: Schema.optionalKey(Schema.String),
  responseSchema: Schema.optionalKey(Schema.Record(Schema.String, Schema.Any)),
  expiresAt: Schema.optionalKey(Schema.String),
  metadata: Schema.optionalKey(Schema.Record(Schema.String, Schema.Any)),
});
const storedInterrupt = Schema.decodeUnknownSync(StoredInterrupt, { onExcessProperty: "preserve" });

/**
 * The turn another request already took for this answer, replayed instead of run again: the thread as it now stands,
 * and the approvals that turn left waiting.
 */
async function* replayed(persistence: Persistence, request: ChatRequest): AsyncGenerator<StreamChunk> {
  const { threadId, runId } = request;
  const stored = await persistence.stores.messages.loadThread(threadId);
  const withIds = stored.map((message, index) => ({ ...message, id: message.id || `snapshot_${runId}_${index}` }));
  yield {
    type: EventType.MESSAGES_SNAPSHOT,
    timestamp: Date.now(),
    messages: uiMessagesToWire(modelMessagesToUIMessages(withIds), { includeSnapshotStructuredOutput: true, includeActivity: true }),
  };
  const waiting = await persistence.stores.interrupts.listPending(threadId);
  const interrupts = waiting.map((record) => storedInterrupt(record.payload));
  yield interrupts.length === 0
    ? { type: EventType.RUN_FINISHED, threadId, runId, finishReason: "stop", timestamp: Date.now() }
    : { type: EventType.RUN_FINISHED, threadId, runId, outcome: { type: "interrupt", interrupts }, timestamp: Date.now() };
}

const CLAIM_POLL_MS = 250;
const CLAIM_RENEW_MS = CLAIM_LEASE_MS / 5;
/** How often a request waiting on another's turn sends something, so no proxy cuts a stream that has gone quiet. */
export const KEEPALIVE_MS = 15_000;

/** A chunk the client ignores: the sidebar handles no custom event by this name. */
const keepalive = (): StreamChunk => ({ type: EventType.CUSTOM, name: "ployz.keepalive", value: null, timestamp: Date.now() });

async function* claimWhenFree(
  run: Run,
  scope: AgentScope,
  claim: string,
  ids: ReadonlyArray<string>,
  signal: AbortSignal | undefined,
): AsyncGenerator<StreamChunk, Effect.Success<ReturnType<typeof claimResume>>> {
  let due = false;
  const beat = setInterval(() => { due = true; }, KEEPALIVE_MS);
  try {
    for (;;) {
      const state = await run(claimResume(scope, claim, ids));
      if (state !== "busy" || signal?.aborted === true) return state;
      if (due) {
        due = false;
        yield keepalive();
      }
      await new Promise((resolve) => setTimeout(resolve, CLAIM_POLL_MS));
    }
  } finally {
    clearInterval(beat);
  }
}

/**
 * One answer runs at most once however many tabs or clients send it: the request that claims the interrupts resumes
 * the turn, and every other one waits for that turn to end and replays it. A turn whose claim another request took over
 * stops at its next commit, and replays the turn that took over.
 */
async function* resumeOnce(
  run: Run,
  persistence: Persistence,
  scope: AgentScope,
  request: ChatRequest & { resume: ReadonlyArray<{ interruptId: string }> },
  turn: (persistence: Persistence) => AsyncIterable<StreamChunk>,
): AsyncGenerator<StreamChunk> {
  const ids = request.resume.map((entry) => entry.interruptId);
  const signal = request.abortController?.signal;
  const claim = randomUUID();
  const state = yield* claimWhenFree(run, scope, claim, ids, signal);
  if (state === "busy") return;
  if (state === "unchecked") return yield* turn(persistence);
  if (state === "settled") {
    yield { type: EventType.RUN_STARTED, threadId: request.threadId, runId: request.runId, timestamp: Date.now() };
    return yield* replayed(persistence, request);
  }
  const renewing = setInterval(() => {
    void run(renewResume(scope, claim).pipe(Effect.catchCause((cause) => Effect.logWarning("Renewing a resumed turn's claim failed.", cause))));
  }, CLAIM_RENEW_MS);
  let finished: StreamChunk | undefined;
  try {
    for await (const chunk of turn(await run(agentPersistence(scope, claim)))) {
      if (chunk.type === EventType.RUN_FINISHED) finished = chunk;
      else yield chunk;
    }
  } catch (error) {
    if (!(error instanceof Superseded)) throw error;
    const next = randomUUID();
    if ((yield* claimWhenFree(run, scope, next, ids, signal)) === "claimed") await run(releaseResume(scope, next));
    return yield* replayed(persistence, request);
  } finally {
    clearInterval(renewing);
    await run(releaseResume(scope, claim));
  }
  if (finished !== undefined) yield finished;
}

async function* startOnce(
  run: Run,
  persistence: Persistence,
  scope: AgentScope,
  request: ChatRequest,
  turn: (persistence: Persistence) => AsyncIterable<StreamChunk>,
): AsyncGenerator<StreamChunk> {
  const { threadId, runId } = request;
  const status = await run(startRun(scope, threadId, runId));
  if (status === "started") return yield* turn(persistence);
  yield { type: EventType.RUN_STARTED, threadId, runId, timestamp: Date.now() };
  if (status !== "foreign" && (status === "interrupted" || isTerminalRunStatus(status))) return yield* replayed(persistence, request);
  const message = status === "foreign" ? "This run belongs to another conversation." : "This message is already being answered.";
  yield { type: EventType.RUN_ERROR, threadId, runId, message, code: "run_exists", timestamp: Date.now() };
}

/**
 * One sidebar turn, or the resumption of one, as `caller` in their thread: the event stream the client renders. `model`
 * replaces the configured one, for the agent eval.
 */
export const agentChat = Effect.fn("Agent.chat")(function* (person: Caller, request: ChatRequest, model?: AnyTextAdapter) {
  const caller: AgentCaller = { ...person, via: "agent" };
  const run: Run = Effect.runPromiseWith(yield* Effect.context<AgentServices>());
  const scope: AgentScope = { organizationId: caller.organization.id, userId: caller.userId };
  const persistence = yield* agentPersistence(scope);
  const { adapter, modelOptions }: AgentModel = model === undefined ? yield* agentModel : { adapter: model };
  const posthog = yield* PostHog;
  const { resume } = request;
  // A page the client sent malformed is no page: the turn still answers.
  const page = resume === undefined ? Option.getOrUndefined(Schema.decodeUnknownOption(PageContext)(request.forwardedProps?.["page"])) : undefined;
  const turn = (stores: Persistence) => {
    const limit = stepLimit(stores);
    const here: Here = { page: { at: "organization" } };
    const base = {
      adapter,
      modelOptions,
      messages: added(request),
      threadId: request.threadId,
      runId: request.runId,
      tools: agentTools(caller, run, request.runId, here),
      systemPrompts: [systemPrompt(caller, adapter.model)],
      middleware: [withPersistence(stores), ...(page === undefined ? [] : [pageMarker(page)]), approvalGate(caller, run, here), limit.middleware,
        agentTracing, aiObservability(posthog, caller)],
      interrupts: [approvalInterrupt],
      agentLoopStrategy: maxIterations(STEP_LIMIT),
    };
    const options: typeof base & { resume?: ChatRequest["resume"]; abortController?: AbortController } = base;
    if (request.abortController !== undefined) options.abortController = request.abortController;
    if (resume !== undefined) options.resume = resume;
    return plainFailures(run, limit.noted(chat(options)));
  };
  if (resume === undefined) return startOnce(run, persistence, scope, request, turn);
  return resumeOnce(run, persistence, scope, { ...request, resume }, turn);
});
