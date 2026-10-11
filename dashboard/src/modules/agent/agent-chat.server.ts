import "@tanstack/react-start/server-only";
import { randomUUID } from "node:crypto";
import type { ConfigCommand, ConfigWritten, DiffView, JsonValue } from "@ployz/sdk";
import {
  type AnyTextAdapter,
  chat,
  type chatParamsFromRequest,
  defineChatMiddleware,
  EventType,
  type Interrupt,
  isTerminalRunStatus,
  type ModelMessage,
  modelMessagesToUIMessages,
  type StreamChunk,
  toolDefinition,
  uiMessagesToWire,
} from "@tanstack/ai";
import { type AnthropicTextProviderOptions, createAnthropicChat } from "@tanstack/ai-anthropic";
import { withPersistence } from "@tanstack/ai-persistence";
import { Config, Effect, Option, Redacted, Schema } from "effect";
import { approvalInterrupt, type ToolOutcome } from "#/modules/agent/agent";
import {
  AGENT,
  AGENT_COMMANDS,
  type AgentBinding,
  type CloudServices,
  inputSchema,
  type StoreBinding,
  toolName,
  uuidFrom,
} from "#/modules/agent/agent-tools";
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
import { latestPageContext, PageContext, renderPageContext, withPageContext } from "#/modules/agent/page-context";
import { notSetUpScript, ScriptedAdapter, stubScript } from "#/modules/agent/scripted-adapter.server";
import { decideApproval, requestApproval, reviewedDiff, trustedApproval } from "#/modules/approvals/approvals.server";
import { callStore } from "#/modules/config-store/config-store.server";
import type { StoreCall, StoreRefusal } from "#/modules/config-store/store.contract";
import type { Caller } from "#/modules/identity/actor";
import { projectJsonValue } from "#/lib/json";

const GATED = new Map(AGENT_COMMANDS.flatMap(({ command, binding }) => binding.kind === "gated" ? [[toolName(command.command), binding]] : []));

type Gated = { outcome: ToolOutcome } | { asking: { approvalId: string; message: string } };
type Run = <A, E>(effect: Effect.Effect<A, E, AgentServices>) => Promise<A>;

const refused = (code: string, message: string): ToolOutcome => ({ ok: false, refusal: { code, message, details: null } });
const invalid = (message: string) => refused("invalid_argument", message);

const storeCall = (binding: StoreBinding, input: ReturnType<typeof projectJsonValue>, turn: string): StoreCall => {
  const raw = input ?? null;
  return binding.kind === "read" ? { operation: "read", query: binding.query(raw) } : { operation: "write", command: binding.command(raw, turn) };
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

const runGated = Effect.fn("Agent.runGated")(function* (caller: Caller, asked: ConfigCommand, approvalId: string | null) {
  const trusted = yield* trustedApproval(caller.organization.id, approvalId);
  if (!trusted.ok) return { outcome: { ok: false, refusal: denialForAgent(asked, trusted.refusal) } } satisfies Gated;
  const reviewed = approvalId === null || trusted.approval === "required" ? null : yield* reviewedDiff(caller.organization.id, approvalId);
  const command = approvalId === null ? asked : onceFor(asked, approvalId, reviewed);
  const result = yield* callStore(caller.organization.id, caller.userId, { operation: "write", command }, AGENT, trusted.approval);
  const landed = result.ok || reviewed === null ? null : landedPublish(command, reviewed, result.refusal);
  if (landed !== null) return { outcome: { ok: true, value: landed } } satisfies Gated;
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
  | CloudServices;

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
  const answers = new Map<string, { approvalId: string | null; cancelled: boolean }>();
  const decide = (call: { id: string; function: { name: string; arguments: string } }, binding: StoreBinding, turn: string) =>
    Effect.gen(function* (): Effect.fn.Return<Gated, Effect.Error<ReturnType<typeof runGated>>, AgentServices> {
      const answer = answers.get(call.id);
      if (answer?.cancelled === true) {
        if (answer.approvalId !== null) {
          yield* decideApproval(caller, answer.approvalId, { reject: {} }).pipe(Effect.catchTag("NotFound", () => Effect.void));
        }
        return { outcome: { ok: false, cancelled: true } };
      }
      const command = yield* Effect.try(() => storeCall(binding, toolInput(call.function.arguments), turn)).pipe(Effect.option);
      if (Option.isNone(command) || command.value.operation !== "write") {
        return { outcome: invalid(`The ${call.function.name} input doesn't match its schema.`) };
      }
      return yield* runGated(caller, command.value.command, answer?.approvalId ?? null);
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
      const decided = await run(decide(only.call, only.binding, ctx.runId));
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

/** A `cloud` binding's answer as the model hears it: its value, or what refused it. */
const cloudOutcome = (binding: Extract<AgentBinding, { kind: "cloud" }>, caller: Caller, input: JsonValue, turn: string) =>
  binding.run(input, caller, turn).pipe(
    Effect.map((value): ToolOutcome => ({ ok: true, value })),
    Effect.catchTags({
      NotFound: (error) => Effect.succeed(refused("not_found", error.message)),
      Validation: (error) => Effect.succeed(invalid(error.message)),
      StoreRefused: (error) => Effect.succeed<ToolOutcome>({ ok: false, refusal: error.refusal }),
    }),
  );

/**
 * What a read, staged write or `cloud` binding answers the model for `input`, the model's own arguments. Input the
 * binding can't parse is the model's mistake, so the model hears it as invalid_argument instead of the run failing.
 */
export const toolOutcome = (
  caller: Caller,
  binding: Exclude<AgentBinding, { kind: "gated" }>,
  input: JsonValue,
  turn: string,
): Effect.Effect<ToolOutcome, Effect.Error<ReturnType<typeof cloudOutcome>> | Effect.Error<ReturnType<typeof callStore>>, AgentServices> => {
  try {
    return binding.kind === "cloud"
      ? cloudOutcome(binding, caller, input, turn)
      : callStore(caller.organization.id, caller.userId, storeCall(binding, input, turn), AGENT);
  } catch (thrown) {
    return Effect.succeed(invalid(thrown instanceof Error ? thrown.message : String(thrown)));
  }
};

/**
 * Every bound Cloud command as a tool. Reads and staged writes go straight to the Store, `cloud` commands to what
 * answers them, and the gate answers gated ones.
 */
const agentTools = (caller: Caller, run: Run, turn: string) => AGENT_COMMANDS.map(({ command, binding }) =>
  toolDefinition({ name: toolName(command.command), description: command.about, inputSchema: inputSchema(command, binding) })
    .server((args) => {
      if (binding.kind === "gated") throw new Error(`${command.command} ran outside the approval gate.`);
      return run(toolOutcome(caller, binding, projectJsonValue(args) ?? null, turn));
    }));

const systemPrompt = (caller: Caller) => `You are the Ployz agent in the sidebar of Ployz Cloud. You act in the Organization "${caller.organization.slug}" as the member who is talking to you, through the same Config Store the ployz CLI uses. Never act in another Organization.

Read before you write: list or inspect what a command touches before you change it. Staged writes (service rm, volume rm, set and the like) only change the next Version; publish and deploy make them real.

Every tool answers { ok: true, value } or { ok: false, refusal: { code, message, details } }. When the Store refuses, tell the member its message verbatim before anything else.

A Publish or Deploy that removes a Volume whose data a Server holds refuses with confirmation_required, naming the Version and the Volumes in details. Retry only when the member explicitly asked for exactly that loss. Preserve the original action, Project and Environment. Pass accept_volume_loss with those Volumes and the complete Version using version for Publish or expect_version for Deploy. Human Approval alone never accepts data loss. Publish saves the removal for a later Deploy and does not delete live data or start a Deployment.

To deploy a GitHub repository: check github_ls first, and when GitHub isn't connected, give the member the link github_connect returns and stop. Read the repository with github_tree (when it comes back truncated, narrow it with match, like **/Dockerfile), then github_cat the files that say how it builds and runs (Dockerfile, compose files, package.json and the like). Create the Project and Environment when there are none, then service_add with repo set to the repository. Add the Configs and domain the code needs, then publish and deploy.

Publish and deploy may wait for a human to approve the plan. Call either one alone, never alongside another tool. When a human denies an approval (approval_denied), quote the reason they gave and do not retry that action or work around it. When an approval is cancelled, say nothing was published or deployed.

A member message may begin with a <dashboard-page …/> block naming the dashboard page they are viewing. The latest block is where they are now. When they say "this service", "here" or name no Project, Environment or Service, use the latest block's. Never mention the block itself.`;

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

type AgentModel = { adapter: AnyTextAdapter; modelOptions?: AnthropicTextProviderOptions };
const scripted = (script: ConstructorParameters<typeof ScriptedAdapter>[0]): AgentModel => ({ adapter: new ScriptedAdapter(script) });

/**
 * The model the sidebar talks to: the scripted stub under `PLOYZ_AGENT_STUB=1`, Claude when Cloud has an Anthropic key.
 * Claude caches the prompt up to its last block, so a turn rereads the thread before it from cache.
 */
const agentModel = Effect.gen(function* () {
  if ((yield* Config.option(Config.string("PLOYZ_AGENT_STUB"))).pipe(Option.contains("1"))) return scripted(stubScript);
  const key = yield* Config.option(Config.redacted("ANTHROPIC_API_KEY"));
  return Option.match(key, {
    onNone: () => scripted(notSetUpScript),
    onSome: (secret): AgentModel => ({
      adapter: createAnthropicChat("claude-sonnet-5-5", Redacted.value(secret)),
      modelOptions: { cache_control: { type: "ephemeral" } },
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

/** One sidebar turn, or the resumption of one, as `caller` in their thread: the event stream the client renders. */
export const agentChat = Effect.fn("Agent.chat")(function* (caller: Caller, request: ChatRequest) {
  const run: Run = Effect.runPromiseWith(yield* Effect.context<AgentServices>());
  const scope: AgentScope = { organizationId: caller.organization.id, userId: caller.userId };
  const persistence = yield* agentPersistence(scope);
  const { adapter, modelOptions } = yield* agentModel;
  const { resume } = request;
  // A page the client sent malformed is no page: the turn still answers.
  const page = resume === undefined ? Option.getOrUndefined(Schema.decodeUnknownOption(PageContext)(request.forwardedProps?.["page"])) : undefined;
  const turn = (stores: Persistence) => {
    const base = {
      adapter,
      modelOptions,
      messages: added(request),
      threadId: request.threadId,
      runId: request.runId,
      tools: agentTools(caller, run, request.runId),
      systemPrompts: [systemPrompt(caller)],
      middleware: [withPersistence(stores), ...(page === undefined ? [] : [pageMarker(page)]), approvalGate(caller, run)],
      interrupts: [approvalInterrupt],
    };
    const options: typeof base & { resume?: ChatRequest["resume"]; abortController?: AbortController } = base;
    if (request.abortController !== undefined) options.abortController = request.abortController;
    if (resume !== undefined) options.resume = resume;
    return chat(options);
  };
  if (resume === undefined) return startOnce(run, persistence, scope, request, turn);
  return resumeOnce(run, persistence, scope, { ...request, resume }, turn);
});
