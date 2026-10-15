import "@tanstack/react-start/server-only";
import type { Act, Answer, Ask, ConfigCommand, ConfigWritten, DiffView, JsonValue, Place, Write } from "@ployz/sdk";
import { Effect, Option, Schema } from "effect";
import type { ToolOutcome } from "#/modules/agent/agent";
import { AGENT, agentResume, agentStart, CATALOG, finish, GATED, untilWrite } from "#/modules/agent/agent-tools";
import { callCloud } from "#/modules/agent/call-cloud.server";
import { type PagePlace, placeForCall } from "#/modules/agent/page-context";
import { requestApproval, reviewedDiff, trustedApproval } from "#/modules/approvals/approvals.server";
import type { Ran } from "#/modules/commands/command.server";
import { findCommand, inputSchema, runCommand } from "#/modules/commands/commands.server";
import { COMMANDS } from "#/modules/commands/registry.server";
import { callStore, type StoreSource } from "#/modules/config-store/config-store.server";
import type { StoreAnswer, StoreCall, StoreRefusal, StoreResult } from "#/modules/config-store/store.contract";
import type { JsonObject } from "#/db/tables";
import type { Caller } from "#/modules/identity/actor";
import { answerForSupport, refusalForSupport } from "#/modules/support-access/support-view";
import { projectJsonValue } from "#/lib/json";
import { uuidFrom } from "#/server/uuid-from";

/** A Caller acting through an agent: the sidebar, or a remote MCP client. */
export type AgentCaller = Omit<Caller, "via"> & { readonly via: "agent" };

/** One tool call. `key` admits it once; `approvalId` is the approval a retry carries; `page` is where it runs. */
export type AgentCall = {
  readonly tool: string;
  readonly input: JsonValue;
  readonly key: string;
  readonly approvalId: string | null;
  readonly page: PagePlace;
};

export type Asking = { readonly approvalId: string; readonly refusal: StoreRefusal };

/** What a call came to: an outcome, or an approval it waits on. */
export type AgentResult = { readonly outcome: ToolOutcome } | { readonly asking: Asking };

export type AgentTool = {
  readonly name: string;
  readonly description: string;
  readonly inputSchema: JsonObject;
  readonly gated: boolean;
  readonly readOnly: boolean;
  /** The input takes a `request_key`, so a client's key is the one the Store admits. */
  readonly keyed: boolean;
};

const keyed = (schema: JsonObject) => Schema.is(Schema.Struct({ properties: Schema.Struct({ request_key: Schema.Unknown }) }))(schema);

/** Every tool an agent may call: the compiled catalog, then each published command offered to agents. */
export const AGENT_TOOLS: ReadonlyArray<AgentTool> = [
  ...CATALOG.tools.map((tool) => {
    // SAFETY: the catalog writes each input schema as a JSON Schema object.
    const schema = tool.input_schema as JsonObject;
    return { name: tool.name, description: tool.description, inputSchema: schema, gated: tool.gate !== "never", readOnly: tool.read_only, keyed: keyed(schema) };
  }),
  ...COMMANDS.filter((command) => command.agents && command.typed === null).map((command) => ({
    name: command.name,
    description: command.about,
    inputSchema: inputSchema(command),
    gated: command.gate !== "never",
    readOnly: command.kind === "read",
    keyed: false,
  })),
];

const refused = (code: string, message: string): ToolOutcome => ({ ok: false, refusal: { code, message, details: null } });

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
 * first, and a save (the Store's publish) of exactly the reviewed version, which the Store refuses as stale once that save
 * committed.
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
    holds_staged: Schema.Boolean,
  }),
});

/**
 * The save `reviewed` approved, when the Store refuses a replay of exactly its version as stale because it already
 * landed: Saved State holds the very Working State the human reviewed. Any other version keeps the Store's conflict.
 */
const landedPublish = (sent: ConfigCommand, reviewed: DiffView, refusal: StoreRefusal): ConfigWritten | null => {
  if (sent.command !== "publish" || sent.version !== reviewed.version || refusal.code !== "conflict") return null;
  const stale = Schema.decodeUnknownOption(StaleReview)(refusal.details);
  if (Option.isNone(stale)) return null;
  const { environment, saved, holds_staged: holdsStaged } = stale.value.diff;
  const same = environment.id === reviewed.environment.id && environment.revision === reviewed.environment.revision;
  return holdsStaged && same ? { written: "published", environment, saved, created: false } : null;
};

/** A Store answer as `caller` may read it: support sees it under the Organization's rules. */
const shownTo = <C extends StoreCall>(caller: AgentCaller, call: C, result: StoreResult<StoreAnswer<C>>) =>
  caller.support === null ? result : answerForSupport(call, result, caller.support);

const asking = (refusal: StoreRefusal): { asking: Asking } | null => {
  const recorded = Schema.decodeUnknownOption(ApprovalAsked)(refusal.details);
  return Option.isNone(recorded) ? null : { asking: { approvalId: recorded.value.approval_id, refusal } };
};

type Gated =
  | { asking: Asking }
  | { denied: StoreRefusal }
  | { result: StoreResult<ConfigWritten>; nothingDestroyed: boolean };

const runGated = Effect.fn("Agent.runGated")(function* (caller: AgentCaller, asked: ConfigCommand, approvalId: string | null) {
  const trusted = yield* trustedApproval(caller.organization.id, approvalId);
  if (!trusted.ok) return { denied: denialForAgent(asked.command === "publish" ? "save" : "deploy", trusted.refusal) } satisfies Gated;
  const reviewed = approvalId === null || trusted.approval === "required" ? null : yield* reviewedDiff(caller.organization.id, approvalId);
  const command = approvalId === null ? asked : onceFor(asked, approvalId, reviewed);
  const call = { operation: "write", command } as const;
  const stored = yield* callStore(caller.organization.id, caller.userId, call, AGENT, trusted.approval);
  const landed = stored.ok || reviewed === null ? null : landedPublish(command, reviewed, stored.refusal);
  if (landed !== null) return { result: { ok: true, value: landed }, nothingDestroyed: false } satisfies Gated;
  const result = shownTo(caller, call, stored);
  if (stored.ok || stored.refusal.code !== "approval_required") {
    return { result, nothingDestroyed: result.ok && trusted.approval === "required" } satisfies Gated;
  }
  const request: StoreRefusal = yield* requestApproval(caller, command, stored.refusal);
  return asking(request) ?? { denied: caller.support === null ? request : refusalForSupport(request, caller.support) } satisfies Gated;
});

const runGatedAct = Effect.fn("Agent.runGatedAct")(function* (caller: AgentCaller, act: Act, approvalId: string | null) {
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

/** A catalog tool whose input takes a Project: it acts in an Environment, defaulting to the place when a call names none. */
const ScopedTool = Schema.Struct({ name: Schema.String, input_schema: Schema.Struct({ properties: Schema.Struct({ project: Schema.Struct({}) }) }) });
const SCOPED = new Set(CATALOG.tools.filter(Schema.is(ScopedTool)).map((tool) => tool.name));
const COMPILED = new Set(CATALOG.tools.map((tool) => tool.name));
const namesProject = Schema.is(Schema.Struct({ project: Schema.String }));

const placeOf = (caller: AgentCaller, page: PagePlace, tool: string, input: JsonValue): { place: Place } | { refusal: ToolOutcome } => {
  const call = placeForCall(page, { scoped: SCOPED.has(tool), namesProject: namesProject(input) });
  if (!call.ok) return { refusal: refused(call.code, call.message) };
  return { place: { project: call.project, environment: call.environment, store: { at: "cloud", organization: caller.organization.id } } };
};

const answered = (result: StoreResult<unknown>): Answer =>
  result.ok ? { value: projectJsonValue(result.value) ?? null } : { error: result.refusal };

const storeAnswer = <C extends StoreCall>(caller: AgentCaller, call: C, source?: StoreSource) =>
  callStore(caller.organization.id, caller.userId, call, source).pipe(Effect.map((result) => answered(shownTo(caller, call, result))));

const agentRead = (caller: AgentCaller) => (ask: Ask) =>
  ask.at === "store"
    ? storeAnswer(caller, { operation: "read", query: ask.ask })
    : callCloud(caller, { ask: ask.ask }).pipe(Effect.map(answered));

const agentWrite = (caller: AgentCaller) => (write: Write) =>
  write.at === "store"
    ? storeAnswer(caller, { operation: "write", command: write.write }, AGENT)
    : callCloud(caller, { act: write.write }).pipe(Effect.map(answered));

const done = (outcome: ToolOutcome): AgentResult => ({ outcome });

/**
 * Runs one agent tool call as `caller`. An ungated tool runs to its own answer. A gated write that waits on a human
 * answers `asking`; the retry carrying that approval runs it. A gated save and a Cloud act let the compiled tool finish;
 * any other gated Store write's answer is the call's outcome.
 */
export const runAgentCall = Effect.fn("Agent.runCall")(function* (caller: AgentCaller, call: AgentCall) {
  const published = findCommand(call.tool);
  if (published !== undefined) {
    if (!published.agents || published.typed !== null) return done(refused("unknown_tool", `No tool is named ${call.tool}.`));
    const ran = yield* runCommand(caller, published.name, call.input, { approvalId: call.approvalId, key: call.key });
    return (!ran.ok && ran.refusal.code === "approval_required" ? asking(ran.refusal) : null) ?? done(heard(ran));
  }
  if (!COMPILED.has(call.tool)) return done(refused("unknown_tool", `No tool is named ${call.tool}.`));
  const at = placeOf(caller, call.page, call.tool, call.input);
  if ("refusal" in at) return done(at.refusal);
  const read = agentRead(caller);
  const started = agentStart(call.tool, call.input, at.place, call.key);
  if (!GATED.has(call.tool)) return done(yield* finish(started, read, agentWrite(caller)));
  const paused = yield* untilWrite(started, read);
  const { step, state } = paused;
  if (step.step !== "write") return done(yield* finish(paused, read, agentWrite(caller)));
  const { write } = step;
  if (write.at === "store") {
    const gated: Gated = yield* runGated(caller, write.write, call.approvalId);
    if ("asking" in gated) return gated;
    if ("denied" in gated) return done({ ok: false, refusal: gated.denied });
    // A save finishes in its tool, so it answers `saved` as native MCP does. A Deployment's answer stays the Store's: the Run card draws it.
    const outcome = write.write.command === "publish" ? yield* finish(agentResume(state, [answered(gated.result)]), read, agentWrite(caller)) : gated.result;
    return done(outcome.ok && gated.nothingDestroyed ? { ...outcome, nothing_destroyed: true } : outcome);
  }
  const acted = yield* runGatedAct(caller, write.write, call.approvalId);
  if ("asking" in acted) return acted;
  return done(yield* finish(agentResume(state, [answered(acted.result)]), read, agentWrite(caller)));
});
