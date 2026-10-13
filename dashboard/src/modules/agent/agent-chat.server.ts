import "@tanstack/react-start/server-only";
import { randomUUID } from "node:crypto";
import { createRequire } from "node:module";
import type * as PloyzSdk from "@ployz/sdk";
import type { Act, Answer, Ask, ConfigCommand, ConfigWritten, DiffView, JsonValue, Place, Write } from "@ployz/sdk";
import {
  type AnyTextAdapter,
  chat,
  type chatParamsFromRequest,
  defineChatMiddleware,
  EventType,
  type Interrupt,
  isTerminalRunStatus,
  type JSONSchema,
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
import { agentTracing, aiObservability } from "#/modules/agent/agent-telemetry.server";
import { callCloud } from "#/modules/agent/call-cloud.server";
import { AGENT, CATALOG, finish, GATED, untilWrite } from "#/modules/agent/agent-tools";
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
import { latestPageContext, PageContext, type PagePlace, placeForCall, placeOfPage, renderPageContext, withPageContext } from "#/modules/agent/page-context";
import { notSetUpScript, ScriptedAdapter, stubScript } from "#/modules/agent/scripted-adapter.server";
import { PostHog } from "#/modules/analytics/posthog.server";
import { decideApproval, requestApproval, reviewedDiff, trustedApproval } from "#/modules/approvals/approvals.server";
import type { Ran } from "#/modules/commands/command.server";
import { findCommand, inputSchema, type Published, runCommand } from "#/modules/commands/commands.server";
import { COMMANDS } from "#/modules/commands/registry.server";
import { callStore, readStore, type StoreSource } from "#/modules/config-store/config-store.server";
import type { StoreAnswer, StoreCall, StoreRefusal, StoreResult } from "#/modules/config-store/store.contract";
import type { Caller } from "#/modules/identity/actor";
import { answerForSupport, refusalForSupport } from "#/modules/support-access/support-view";
import { projectJsonValue } from "#/lib/json";
import { uuidFrom } from "#/server/uuid-from";

// SAFETY: the package exports this named CommonJS SDK surface at runtime.
const { agentResume, agentStart } = createRequire(import.meta.url)("@ployz/sdk") as Pick<typeof PloyzSdk, "agentResume" | "agentStart">;

type Gated = { outcome: ToolOutcome } | { asking: { approvalId: string; message: string } };
type Run = <A, E>(effect: Effect.Effect<A, E, AgentServices>) => Promise<A>;

const refused = (code: string, message: string): ToolOutcome => ({ ok: false, refusal: { code, message, details: null } });
const invalid = (message: string) => refused("invalid_argument", message);

const ApprovalAsked = Schema.Struct({ approval_id: Schema.String });
const DeniedApproval = Schema.Struct({ approval: Schema.Struct({ reason: Schema.NullOr(Schema.String) }) });

/** A denial as the agent hears it: what was denied and the human's reason, without the approval's ID to echo. */
const denialForAgent = (what: string, refusal: StoreRefusal): StoreRefusal => {
  if (refusal.code !== "approval_denied") return refusal;
  const reason = Option.match(Schema.decodeUnknownOption(DeniedApproval)(refusal.details), { onNone: () => null, onSome: ({ approval }) => approval.reason });
  return { code: refusal.code, message: `A human denied this ${what}${reason === null ? "." : `: ${reason}`}`, details: { approval: { reason } } };
};

const ACTS = {
  service: "Service act",
  drain: "drain",
  upgrade: "upgrade",
  set_server: "server policy change",
  clean: "cleanup",
  remove_server: "server removal",
  forget_server: "server forget",
  volume: "Volume run",
} as const satisfies Record<Act["action"]["action"], string>;

/** A UUID that only `approvalId` produces, so a second Deploy under one approval replays the first instead of running. */
const approvalUuid = (approvalId: string) => uuidFrom(`ployz.agent.deploy:${approvalId}`);

/**
 * `command` pinned to what one approval allows exactly once: a Deploy keyed by the approval, so a second one replays the
 * first, and a Publish of exactly the reviewed version, which the Store refuses as stale once that Publish committed.
 */
const onceFor = (command: ConfigCommand, approvalId: string, reviewed: DiffView | null): ConfigCommand => {
  if (command.command === "admit") return { ...command, id: approvalUuid(approvalId) };
  if (command.command === "publish" && command.version == null && reviewed !== null) return { ...command, version: reviewed.version };
  return command;
};

const StaleReview = Schema.Struct({
  diff: Schema.Struct({
    environment: Schema.Struct({ id: Schema.String, project: Schema.String, name: Schema.String, revision: Schema.Number }),
    saved: Schema.NullOr(Schema.Number),
    published: Schema.Boolean,
  }),
});

/**
 * The Publish `reviewed` approved, when the Store refuses a replay of exactly its version as stale because it already
 * landed: Saved State holds the very Working State the human reviewed. Any other version keeps the Store's conflict.
 */
const landedPublish = (sent: ConfigCommand, reviewed: DiffView, refusal: StoreRefusal): ConfigWritten | null => {
  if (sent.command !== "publish" || sent.version !== reviewed.version || refusal.code !== "conflict") return null;
  const stale = Schema.decodeUnknownOption(StaleReview)(refusal.details);
  if (Option.isNone(stale)) return null;
  const { environment, saved, published } = stale.value.diff;
  const same = environment.id === reviewed.environment.id && environment.revision === reviewed.environment.revision;
  return published && same ? { written: "published", environment, saved, created: false } : null;
};

/** A Store answer as `caller` may read it: support sees it under the Organization's rules. */
const shownTo = <C extends StoreCall>(caller: Caller, call: C, result: StoreResult<StoreAnswer<C>>) =>
  caller.support === null ? result : answerForSupport(call, result, caller.support);

const runGated = Effect.fn("Agent.runGated")(function* (caller: Caller, asked: ConfigCommand, approvalId: string | null) {
  const trusted = yield* trustedApproval(caller.organization.id, approvalId);
  if (!trusted.ok) return { outcome: { ok: false, refusal: denialForAgent(asked.command === "publish" ? "publish" : "deploy", trusted.refusal) } } satisfies Gated;
  const reviewed = approvalId === null || trusted.approval === "required" ? null : yield* reviewedDiff(caller.organization.id, approvalId);
  const command = approvalId === null ? asked : onceFor(asked, approvalId, reviewed);
  const call = { operation: "write", command } as const;
  const stored = yield* callStore(caller.organization.id, caller.userId, call, AGENT, trusted.approval);
  const landed = stored.ok || reviewed === null ? null : landedPublish(command, reviewed, stored.refusal);
  if (landed !== null) return { outcome: { ok: true, value: landed } } satisfies Gated;
  const result = shownTo(caller, call, stored);
  if (result.ok && trusted.approval === "required") return { outcome: { ...result, nothing_destroyed: true } } satisfies Gated;
  if (stored.ok || stored.refusal.code !== "approval_required") return { outcome: result } satisfies Gated;
  const request: StoreRefusal = yield* requestApproval(caller, command, stored.refusal);
  return asking(request) ?? { outcome: { ok: false, refusal: caller.support === null ? request : refusalForSupport(request, caller.support) } } satisfies Gated;
});

const asking = (refusal: StoreRefusal): Extract<Gated, { asking: unknown }> | null => {
  const recorded = Schema.decodeUnknownOption(ApprovalAsked)(refusal.details);
  return Option.isNone(recorded) ? null : { asking: { approvalId: recorded.value.approval_id, message: refusal.message } };
};

const runGatedAct = Effect.fn("Agent.runGatedAct")(function* (caller: Caller, act: Act, approvalId: string | null) {
  const result = yield* callCloud(caller, { act }, approvalId);
  if (result.ok) return { result };
  if (result.refusal.code === "approval_required") {
    const asked = asking(result.refusal);
    if (asked !== null) return asked;
  }
  return { result: { ok: false, refusal: denialForAgent(ACTS[act.action.action], result.refusal) } as const };
});

/** A published command's answer as the model hears it; the line `ployz` prints for a human is not for the model. */
const heard = (ran: Ran<unknown>): ToolOutcome => ran.ok ? { ok: true, value: ran.output } : { ok: false, refusal: ran.refusal };

type AgentServices =
  | Effect.Services<ReturnType<typeof runGated>>
  | Effect.Services<ReturnType<typeof runGatedAct>>
  | Effect.Services<ReturnType<typeof runCommand>>
  | Effect.Services<ReturnType<typeof agentPersistence>>;

const decodeArguments = Schema.decodeUnknownOption(Schema.fromJsonString(Schema.Unknown));
const toolInput = (args: string) => projectJsonValue(Option.getOrElse(decodeArguments(args.trim() === "" ? "{}" : args), () => null)) ?? null;

const placeIn = (caller: Caller, messages: ReadonlyArray<ModelMessage>) => {
  const organizationId = caller.organization.id;
  return placeOfPage(
    latestPageContext(messages),
    readStore(organizationId, { query: "projects" }).pipe(Effect.map(({ projects }) => projects.map(({ name }) => name))),
    (project) => readStore(organizationId, { query: "environments", project }).pipe(
      Effect.map((view) => ({ project: view.project.name, environments: view.environments })),
    ),
  );
};

/** A catalog tool whose input takes a Project: it acts in an Environment, defaulting to the place when a call names none. */
const ScopedTool = Schema.Struct({ name: Schema.String, input_schema: Schema.Struct({ properties: Schema.Struct({ project: Schema.Struct({}) }) }) });
const SCOPED = new Set(CATALOG.tools.filter(Schema.is(ScopedTool)).map((tool) => tool.name));
const namesProject = Schema.is(Schema.Struct({ project: Schema.String }));

type Here = { page: PagePlace };

/** The place `tool` runs `input` at from the page in `here`, or the refusal the model hears instead. */
const placeOf = (caller: Caller, here: Here, tool: string, input: JsonValue): { place: Place } | { refusal: ToolOutcome } => {
  const call = placeForCall(here.page, { scoped: SCOPED.has(tool), namesProject: namesProject(input) });
  if (!call.ok) return { refusal: refused(call.code, call.message) };
  return { place: { project: call.project, environment: call.environment, store: { at: "cloud", organization: caller.organization.id } } };
};

const answered = (result: StoreResult<unknown>): Answer =>
  result.ok ? { value: projectJsonValue(result.value) ?? null } : { error: result.refusal };

const storeAnswer = <C extends StoreCall>(caller: Caller, call: C, source?: StoreSource) =>
  callStore(caller.organization.id, caller.userId, call, source).pipe(Effect.map((result) => answered(shownTo(caller, call, result))));

const agentRead = (caller: Caller) => (ask: Ask) =>
  ask.at === "store"
    ? storeAnswer(caller, { operation: "read", query: ask.ask })
    : callCloud(caller, { ask: ask.ask }).pipe(Effect.map(answered));

const agentWrite = (caller: Caller) => (write: Write) =>
  write.at === "store"
    ? storeAnswer(caller, { operation: "write", command: write.write }, AGENT)
    : callCloud(caller, { act: write.write }).pipe(Effect.map(answered));

export const runTool = (caller: Caller, tool: string, input: JsonValue, place: Place, callKey: string) =>
  finish(agentStart(tool, input, place, callKey), agentRead(caller), agentWrite(caller));

/** The latest assistant turn's tool calls that have no result yet: what the tool phase is about to run. */
const pendingCalls = (messages: ReadonlyArray<ModelMessage>) => {
  const turn = messages.map((message) => message.role === "assistant" && (message.toolCalls?.length ?? 0) > 0).lastIndexOf(true);
  const answered = new Set(messages.slice(turn + 1).flatMap((message) => message.role === "tool" && message.toolCallId !== undefined ? [message.toolCallId] : []));
  return (messages[turn]?.toolCalls ?? []).filter((call) => !answered.has(call.id));
};

const approvalGate = (caller: Caller, run: Run, here: Here) => {
  const outcomes = new Map<string, ToolOutcome>();
  const answers = new Map<string, { approvalId: string | null; cancelled: boolean }>();
  const decide = (call: { id: string; function: { name: string; arguments: string } }, turn: string) =>
    Effect.gen(function* (): Effect.fn.Return<Gated, Effect.Error<ReturnType<typeof runGated>> | Effect.Error<ReturnType<typeof runCommand>>, AgentServices> {
      const answer = answers.get(call.id);
      if (answer?.cancelled === true) {
        if (answer.approvalId !== null) {
          yield* decideApproval(caller, answer.approvalId, { reject: {} }).pipe(Effect.catchTag("NotFound", () => Effect.void));
        }
        return { outcome: { ok: false, cancelled: true } };
      }
      const published = findCommand(call.function.name);
      if (published !== undefined) {
        const ran = yield* runCommand(caller, published.name, toolInput(call.function.arguments), { approvalId: answer?.approvalId ?? null, key: `${turn}:${call.id}` });
        return (!ran.ok && ran.refusal.code === "approval_required" ? asking(ran.refusal) : null) ?? { outcome: heard(ran) };
      }
      const read = agentRead(caller);
      const input = toolInput(call.function.arguments);
      const at = placeOf(caller, here, call.function.name, input);
      if ("refusal" in at) return { outcome: at.refusal };
      const started = agentStart(call.function.name, input, at.place, `${turn}:${call.id}`);
      const paused = yield* untilWrite(started, read);
      const { step, state } = paused;
      if (step.step !== "write") return { outcome: yield* finish(paused, read, agentWrite(caller)) };
      const { write } = step;
      if (write.at === "store") return yield* runGated(caller, write.write, answer?.approvalId ?? null);
      const acted = yield* runGatedAct(caller, write.write, answer?.approvalId ?? null);
      if ("asking" in acted) return acted;
      return { outcome: yield* finish(agentResume(state, [answered(acted.result)]), read, agentWrite(caller)) };
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

const publishedTool = (caller: Caller, run: Run, turn: string) => (command: Published) =>
  toolDefinition({ name: command.name, description: command.about, inputSchema: inputSchema(command) })
    .server((args, context) => {
      if (GATED.has(command.name)) throw new Error(`${command.name} ran outside the approval gate.`);
      const key = `${turn}:${context?.toolCallId ?? randomUUID()}`;
      return run(runCommand(caller, command.name, projectJsonValue(args) ?? null, { approvalId: null, key }).pipe(Effect.map(heard)));
    });

/** The compiled catalog's tools, then every published command Cloud offers agents. */
const agentTools = (caller: Caller, run: Run, turn: string, here: Here) => [
  ...CATALOG.tools.map((tool) =>
    // SAFETY: the catalog writes each input schema as a JSON Schema object.
    toolDefinition({ name: tool.name, description: tool.description, inputSchema: tool.input_schema as JSONSchema })
      .server((args, context) => {
        if (GATED.has(tool.name)) throw new Error(`${tool.name} ran outside the approval gate.`);
        const input = projectJsonValue(args) ?? null;
        const at = placeOf(caller, here, tool.name, input);
        if ("refusal" in at) return at.refusal;
        return run(runTool(caller, tool.name, input, at.place, `${turn}:${context?.toolCallId ?? randomUUID()}`));
      })),
  ...COMMANDS.filter((command) => command.agents).map(publishedTool(caller, run, turn)),
];

const systemPrompt = (caller: Caller, model: string) => `${CATALOG.instructions}

You are ${model}, the Ployz agent in the sidebar of Ployz Cloud. You act in the Organization "${caller.organization.slug}" as the member who is talking to you. Never act in another Organization.

Your place is the Project and Environment named by the latest <dashboard-page …/> block, which opens a member message whenever they move. When they say "this service" or "here", use that block's. Never mention the block itself. With no block, the Organization's defaults apply.

A Publish, a Deploy, removing an Environment or a Project, a server drain, cleanup, removal or forget, and a Volume mirror, mirror removal, move or release may wait for a human to approve the plan. Call any of them alone, never alongside another tool. When a human denies an approval (approval_denied), quote the reason they gave and do not retry that action or work around it. When an approval is cancelled, say nothing changed. A Service restart, start or stop never waits, and neither does a server upgrade.`;

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
 * Model turns one member message may take. Deploying a GitHub repository takes 14 before its Publish waits on a human,
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
  // SAFETY: the interrupt store keeps each interrupt exactly as the run that raised it published it.
  const interrupts = waiting.map((record) => record.payload as Interrupt);
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
  const caller: Caller = { ...person, via: "agent" };
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
