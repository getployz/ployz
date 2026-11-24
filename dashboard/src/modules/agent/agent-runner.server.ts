import "@tanstack/react-start/server-only";
import type { Act, Answer, Ask, ConfigCommand, ConfigWritten, JsonValue, Place, Write } from "@ployz/sdk";
import { Cause, Effect, Schema } from "effect";
import type { ToolOutcome } from "#/modules/agent/agent";
import { agentResume, agentStart, CATALOG, finish, GATED, untilWrite } from "#/modules/agent/agent-tools";
import { callCloud } from "#/modules/agent/call-cloud.server";
import { type PagePlace, placeForCall } from "#/modules/agent/page-context";
import { approvalAsks } from "#/modules/config-store/store-asks";
import { gateOperation, hasApprover, needsPerson } from "#/modules/operations/gate.server";
import { type Called, type Command, inputSchema } from "#/modules/commands/define";
import { findCommand, runCommand } from "#/modules/commands/commands.server";
import { COMMANDS } from "#/modules/commands/registry.server";
import { callStore } from "#/modules/config-store/config-store.server";
import { agentSecretRefusal, holdsSecretText } from "#/modules/agent/agent-secrets";
import { protectorOnly, unprotectAsked } from "#/modules/config-store/protected.server";
import type { StoreAnswer, StoreCall, StoreResult } from "#/modules/config-store/store.contract";
import { refused } from "#/lib/refusal";
import type { JsonObject } from "#/db/tables";
import { type AgentCaller, callerEnvironments, callerPermissions } from "#/modules/identity/actor";
import { type DeployLinked, linkDeploy } from "#/modules/deploy-approval/deploy-link.server";
import { createProposal, presentProposal, proposalStatus, stageStep } from "#/modules/proposals/proposals.server";
import { answerForSupport } from "#/modules/support-access/support-view";
import { asRecord, asString, projectJsonValue } from "#/lib/json";
import { type CallError, type CallOrigin, errorTypeOf, recordAgentCall, reporterOf, type WhatHappened } from "#/modules/agent/agent-calls.server";

/**
 * One tool call. `key` admits it once; `page` is where it runs; `origin` is the surface it came through. `input` still holds the `intent` the agent sent; `runAgentCall` takes it out.
 */
export type AgentCall = {
  readonly tool: string;
  readonly input: JsonValue;
  readonly key: string;
  readonly page: PagePlace;
  readonly origin: CallOrigin;
  /** The surface ended the call itself, before it could run, with this error: it is recorded as the failed call it is, and does not run. */
  readonly ended?: CallError;
};

export type AgentResult = { readonly outcome: ToolOutcome };

export type AgentTool = {
  readonly name: string;
  /** A short label a host shows people; the model reads `description`. */
  readonly title: string;
  readonly description: string;
  readonly inputSchema: JsonObject;
  readonly gated: boolean;
  readonly readOnly: boolean;
  /** It removes or deletes something when it runs; a staged removal is not destructive. */
  readonly destructive: boolean;
  /** The same call made again, with the same input, changes nothing more. */
  readonly idempotent: boolean;
  /** It reaches past Ployz when it runs, such as an address on the internet. */
  readonly openWorld: boolean;
  /** The input takes a `request_key`, so a client's key is the one the Store admits. */
  readonly keyed: boolean;
};

const keyed = (schema: JsonObject) => Schema.is(Schema.Struct({ properties: Schema.Struct({ request_key: Schema.Unknown }) }))(schema);

const NAME = { type: "string", pattern: "^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$" };
const PROPOSAL_ID = { type: "string", pattern: "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$" };
const PROPOSAL = {
  ...PROPOSAL_ID,
  description: "Stage this call as the next step of a proposal instead of running it: nothing runs until a human approves the whole proposal.",
};

/** The Service tools a proposal may hold a step of: the ones a Server checks against the approval a person signed. */
export const PROPOSAL_TOOLS = new Set(["service_restart", "service_stop", "service_start"]);
const WITH_PROPOSAL = new Set(PROPOSAL_TOOLS);
const STAGE_SEVERAL = "For several steps under one approval, stage them in a proposal.";
/** What the catalog's gated tools end with, said of a published command that asks too: its `about` is the CLI's help. */
export const ASKS = "May need a person.";
/** A proposal's place in a Service tool: a gated call already says it asks, an ungated one asks only as a step. */
const inProposal = (gated: boolean): string =>
  gated ? STAGE_SEVERAL : `It runs without approval; as a step of a proposal, a human approves it in the dashboard. ${STAGE_SEVERAL}`;

/** Tools that build a proposal: several Service restarts, stops and starts a human approves together, in order. */
const PROPOSAL_STEPS: ReadonlyArray<AgentTool> = [
  {
    name: "proposal_create",
    title: "Open a proposal",
    description: "Open an empty proposal in an Environment: a plan of several Service restarts, stops and starts that a human approves together. Stage each step by calling its Service tool with proposal set to the ID this returns, in the order to run, then hand it over with `proposal_present`. If a step fails, the ones after it never run.",
    inputSchema: { type: "object", additionalProperties: false, properties: { project: { ...NAME, description: "The Environment's Project, by name, e.g. `shop`." }, environment: { ...NAME, description: "The Environment to open it in, by name, e.g. `staging`." } } },
    gated: false,
    readOnly: false,
    destructive: false,
    idempotent: false,
    openWorld: false,
    keyed: false,
  },
  {
    name: "proposal_present",
    title: "Present a proposal for approval",
    description: "Hand a proposal to the people who can approve it. It needs at least one step, and nothing more can be staged into it afterwards. Check progress with `proposal_status`.",
    inputSchema: { type: "object", additionalProperties: false, properties: { proposal: { ...PROPOSAL_ID, description: "The proposal's ID, from the call that opened it." } }, required: ["proposal"] },
    gated: false,
    readOnly: false,
    destructive: false,
    // A repeat is refused: the proposal was already presented.
    idempotent: true,
    openWorld: false,
    keyed: false,
  },
  {
    name: "proposal_status",
    title: "Show a proposal's status",
    description: "Show where a proposal stands: waiting on a human, denied with their reason, or how far each step got.",
    inputSchema: { type: "object", additionalProperties: false, properties: { proposal: { ...PROPOSAL_ID, description: "The proposal's ID, from the call that opened it." } }, required: ["proposal"] },
    gated: false,
    readOnly: true,
    destructive: false,
    idempotent: true,
    openWorld: false,
    keyed: false,
  },
];

/** What every tool asks of its caller, so each call says what it is for. The same text is on `ployz mcp`'s tools. */
export const INTENT_DESCRIPTION = "Why are you calling this tool? Describe the goal in general terms, and refer to people by role (\"a customer\") rather than by name. Leave out personal and identifying information.";
export const INTENT_REFUSAL = "Say why you are calling this tool. Send `intent`, a short sentence on the goal this call serves.";

/** A tool's input with `intent` added and required, as `approval` is added where a client's tool list is built. */
const withIntent = (tool: AgentTool): AgentTool => {
  const schema = tool.inputSchema;
  const properties = asRecord(schema["properties"]) ?? {};
  const required = Array.isArray(schema["required"]) ? schema["required"] : [];
  return { ...tool, inputSchema: { ...schema, properties: { ...properties, intent: { type: "string", description: INTENT_DESCRIPTION } }, required: [...required, "intent"] } };
};

/** Every tool an agent may call: the compiled catalog, then each published command offered to agents, then proposals. */
export const AGENT_TOOLS: ReadonlyArray<AgentTool> = [
  ...CATALOG.tools.map((tool) => {
    // SAFETY: the catalog writes each input schema as a JSON Schema object.
    const schema = tool.input_schema as JsonObject;
    const properties = asRecord(schema["properties"]) ?? {};
    const inputSchema = WITH_PROPOSAL.has(tool.name) ? { ...schema, properties: { ...properties, proposal: PROPOSAL } } : schema;
    const description = WITH_PROPOSAL.has(tool.name) ? `${tool.description} ${inProposal(tool.gate !== "never")}` : tool.description;
    return {
      name: tool.name,
      title: tool.title,
      description,
      inputSchema,
      gated: tool.gate !== "never",
      readOnly: tool.read_only,
      destructive: tool.destructive,
      // Sent again with a proposal, a Service tool stages a second step.
      idempotent: tool.idempotent && !WITH_PROPOSAL.has(tool.name),
      openWorld: tool.open_world,
      keyed: keyed(schema),
    };
  }),
  ...COMMANDS.filter((command) => command.agents && command.typed === null).map((command) => ({
    name: command.name,
    title: command.title,
    description: command.gate === "never" ? command.about : `${command.about.replace(/\.$/, "")}. ${ASKS}`,
    inputSchema: inputSchema(command.input),
    gated: command.gate !== "never",
    readOnly: command.kind === "read",
    destructive: command.removes,
    idempotent: command.idempotent,
    openWorld: command.openWorld,
    keyed: false,
  })),
  ...PROPOSAL_STEPS,
].map(withIntent);

const KNOWN_TOOLS = new Set(AGENT_TOOLS.map((tool) => tool.name));

export const stagesInto = (input: JsonValue): string | null => asString(asRecord(input)?.["proposal"]);

/** A Store answer as `caller` may read it: support sees it under the Organization's rules. */
const shownTo = <C extends StoreCall>(caller: AgentCaller, call: C, result: StoreResult<StoreAnswer<C>>) =>
  caller.support === null ? result : answerForSupport(call, result, caller.support);

type Gated = { linked: DeployLinked } | { result: StoreResult<ConfigWritten>; nothingDestroyed: boolean };

/**
 * A gated Store write as `caller`: refused if it carries a secret, a Deploy linked for a person to approve, else sent to
 * the Store. A write the Store asks a person about is refused with where one does it: no agent says yes.
 */
export const runGated = Effect.fn("Agent.runGated")(function* (caller: AgentCaller, asked: ConfigCommand) {
  // No person approves a secret through an agent.
  const secret = agentSecretRefusal(asked);
  if (secret !== null) return { result: { ok: false, refusal: secret }, nothingDestroyed: false } satisfies Gated;
  // A Deploy a person must approve becomes one proposal with one link.
  if (asked.command === "admit" && asked.admit === "deploy") {
    const link = yield* linkDeploy(caller, asked);
    if (link !== null) return "refused" in link ? { result: { ok: false, refusal: link.refused }, nothingDestroyed: false } satisfies Gated : { linked: link.linked } satisfies Gated;
  }
  const approval = (yield* hasApprover(caller.organization.id)) ? "required" : "not_required";
  const call = { operation: "write", command: asked } as const;
  const stored = yield* callStore(caller.organization.id, caller.userId, call, { approval, asks: approvalAsks(caller.via, asked), permissions: callerPermissions(caller), environments: callerEnvironments(caller) });
  if (!stored.ok && stored.refusal.code === "approval_required") return { result: { ok: false, refusal: needsPerson(asked, stored.refusal) }, nothingDestroyed: false } satisfies Gated;
  const result = shownTo(caller, call, stored);
  return { result, nothingDestroyed: result.ok && approval === "required" } satisfies Gated;
});

const LinkedAct = Schema.Struct({ proposal_id: Schema.String, url: Schema.String });

/** A Cloud act as `caller`. A stop or restart a person must approve answers the link to its proposal, as a Deploy does. */
const runGatedAct = Effect.fn("Agent.runGatedAct")(function* (caller: AgentCaller, act: Act) {
  const result = yield* callCloud(caller, { act });
  if (result.ok || result.refusal.code !== "conflict" || !Schema.is(LinkedAct)(result.refusal.details)) return { result };
  const { proposal_id, url } = result.refusal.details;
  return { linked: { proposal_id, status: "presented", url, message: result.refusal.message } satisfies DeployLinked };
});

/** The call to repeat after `command` refused: the fields it takes, so a key the caller made up is not echoed. */
const repeat = (command: Pick<Command, "input">, call: AgentCall): JsonObject =>
  Object.fromEntries(Object.entries(asRecord(call.input) ?? {}).filter(([key]) => Object.hasOwn(command.input.fields, key)));

/**
 * A published command's answer as the model hears it, its refusal carrying the call to repeat as a Store tool's does,
 * unless that call holds a secret's text; the line `ployz` prints for a human is not for the model.
 */
const heard = (command: Pick<Command, "input">, call: AgentCall, ran: Called<unknown>): ToolOutcome => {
  if (ran.ok) return { ok: true, value: ran.output };
  const input = repeat(command, call);
  return { ok: false, refusal: holdsSecretText(input) ? ran.refusal : { ...ran.refusal, retry: { tool: call.tool, input } } };
};

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

const storeAnswer = <C extends StoreCall>(caller: AgentCaller, call: C) => {
  const secret = call.operation === "write" ? agentSecretRefusal(call.command) : null;
  return secret !== null ? Effect.succeed(answered({ ok: false, refusal: secret })) : callStore(caller.organization.id, caller.userId, call, { approval: "not_required", permissions: callerPermissions(caller), environments: callerEnvironments(caller) }).pipe(Effect.map((result) => answered(shownTo(caller, call, result))));
};

const agentRead = (caller: AgentCaller) => (ask: Ask) => Effect.gen(function* () {
  return ask.at === "store"
    ? yield* storeAnswer(caller, { operation: "read", query: ask.ask })
    : answered(yield* callCloud(caller, { ask: ask.ask }));
});

const agentWrite = (caller: AgentCaller) => (write: Write) => Effect.gen(function* () {
  return write.at === "store"
    ? yield* storeAnswer(caller, { operation: "write", command: write.write })
    : answered(yield* callCloud(caller, { act: write.write }));
});

const done = (outcome: ToolOutcome): AgentResult => ({ outcome });

const PROPOSAL_NAMES = new Set(PROPOSAL_STEPS.map((tool) => tool.name));

const runProposalTool = Effect.fn("Agent.runProposalTool")(function* (caller: AgentCaller, call: AgentCall, proposal: string | null) {
  if (call.tool === "proposal_create") {
    const at = placeForCall(call.page, { scoped: true, namesProject: namesProject(call.input) });
    if (!at.ok) return refused(at.code, at.message);
    const input = asRecord(call.input) ?? {};
    const project = asString(input["project"]) ?? at.project;
    const environment = asString(input["environment"]) ?? at.environment;
    return yield* createProposal(caller, { project, environment });
  }
  if (proposal === null) return refused("invalid_argument", `This input does not fit: proposal is a proposal's id, a string.`);
  return call.tool === "proposal_present" ? yield* presentProposal(caller, proposal) : yield* proposalStatus(caller, proposal);
});

/** Runs a Service tool as far as the Cloud act it would send, then stages that act into `proposal` instead of sending it. */
const stageCall = Effect.fn("Agent.stageCall")(function* (caller: AgentCaller, call: AgentCall, proposal: string) {
  const { proposal: _proposal, ...input } = asRecord(call.input) ?? {};
  const at = placeOf(caller, call.page, call.tool, input);
  if ("refusal" in at) return at.refusal;
  const { step } = yield* untilWrite(agentStart(call.tool, input, at.place, call.key), agentRead(caller));
  // The tool's own answer, such as a Service it can't find, before it would act.
  if (step.step === "done") return step.result;
  if (step.write.at !== "act" || step.write.write.action.action !== "service") return refused("invalid_argument", `${call.tool} found nothing to stage.`);
  const { environment, service, verb, only } = step.write.write.action;
  return yield* stageStep(caller, proposal, { environment, service, verb, only });
});

/** The reason an agent gave for a call, and the input without it; null when it gave none, or only blanks. */
export const takeIntent = (input: JsonValue) => {
  const record = asRecord(input);
  if (record === null) return { intent: null, input };
  const { intent, ...rest } = record;
  const said = asString(intent)?.trim();
  return { intent: said === undefined || said === "" ? null : said, input: rest };
};

/** How a finished call went, for the record: null when it did what was asked. */
const failureOf = (outcome: ToolOutcome): CallError | null => {
  if (outcome.ok) return null;
  if ("cancelled" in outcome) return { type: "cancelled", message: "The call was cancelled." };
  return { type: errorTypeOf(outcome.refusal.code), message: outcome.refusal.message };
};

/**
 * Runs one agent tool call as `caller`, and records it. Every tool takes an `intent`: the reason for the call, taken out
 * before the tool sees its input. A call without one is refused.
 */
export const runAgentCall = Effect.fn("Agent.runCall")(function* (caller: AgentCaller, call: AgentCall) {
  const startedAt = new Date();
  const record = (what: WhatHappened) =>
    recordAgentCall({
      ...call.origin,
      ...what,
      startedAt,
      durationMs: Date.now() - startedAt.getTime(),
      caller: reporterOf(caller),
    });
  if (!KNOWN_TOOLS.has(call.tool)) {
    yield* record({ kind: "unknownTool", tool: call.tool });
    return done(refused("unknown_tool", `No tool is named ${call.tool}.`));
  }
  const { intent, input } = takeIntent(call.input);
  if (call.ended !== undefined) {
    yield* record({ kind: "call", tool: call.tool, intent, error: call.ended });
    return done(call.ended.type === "cancelled" ? { ok: false, cancelled: true } : refused("invalid_argument", call.ended.message));
  }
  if (intent === null) {
    yield* record({ kind: "call", tool: call.tool, intent: null, error: { type: "validation", message: INTENT_REFUSAL } });
    return done(refused("invalid_argument", INTENT_REFUSAL));
  }
  // A tool that fails as an Effect failure is a call that failed too: record it, with no word of the cause, and fail the same.
  // A call that was only interrupted did not fail, so it is not one.
  const result = yield* runTool(caller, { ...call, input }).pipe(
    Effect.tapCause((cause) =>
      Cause.hasInterruptsOnly(cause) ? Effect.void : record({ kind: "call", tool: call.tool, intent, error: { type: "internal", message: "The tool failed." } }),
    ),
  );
  yield* record({ kind: "call", tool: call.tool, intent, error: failureOf(result.outcome) });
  return result;
});

/**
 * Runs one agent tool call as `caller`. An ungated tool runs to its own answer. A Deploy, stop or restart a person must
 * approve answers the link to its proposal; any other write a person must say yes to is refused with where one does
 * it. A gated save and a Cloud act let the compiled tool finish; any other gated Store write's answer is the call's outcome.
 */
const runTool = Effect.fn("Agent.runTool")(function* (caller: AgentCaller, call: AgentCall) {
  const proposal = stagesInto(call.input);
  if (PROPOSAL_NAMES.has(call.tool)) return done(yield* runProposalTool(caller, call, proposal));
  if (proposal !== null) {
    if (!PROPOSAL_TOOLS.has(call.tool)) return done(refused("invalid_argument", `Only service_restart, service_stop and service_start can be staged into a proposal. Call ${call.tool} without proposal.`));
    return done(yield* stageCall(caller, call, proposal));
  }
  const published = findCommand(call.tool);
  if (published !== undefined) {
    if (!published.agents || published.typed !== null) return done(refused("unknown_tool", `No tool is named ${call.tool}.`));
    const ran = yield* runCommand(caller, published.name, call.input, { approved: null, key: call.key });
    return done(heard(published, call, ran));
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
  // Protecting asks no one; unprotecting needs a person, so an agent is refused with where one does it.
  if (write.at === "store" && write.write.command === "set_environment_protected") {
    const unprotect = yield* unprotectAsked(caller.organization.id, write.write).pipe(
      Effect.catchTag("StoreRefused", (error) => Effect.succeed({ refused: error.refusal })),
    );
    if ("refused" in unprotect) return done({ ok: false, refusal: unprotect.refused });
    const { asked, command } = unprotect;
    if (asked !== null) {
      const refused = protectorOnly(caller, asked.preview.environment);
      if (refused !== null) return done({ ok: false, refusal: refused });
      const gated = yield* gateOperation(caller, null, { ...asked, ployz: `ployz env protect ${asked.preview.environment} --off` });
      if (!gated.ok) return done({ ok: false, refusal: gated.refusal });
    }
    const written = yield* storeAnswer(caller, { operation: "write", command });
    return done(yield* finish(agentResume(state, [written]), read, agentWrite(caller)));
  }
  if (write.at === "store") {
    const gated: Gated = yield* runGated(caller, write.write);
    if ("linked" in gated) return done({ ok: true, value: { ...gated.linked, next: `Don't deploy again. Call proposal_status with proposal ${gated.linked.proposal_id} to follow it.` } });
    // A save and a refusal finish in the tool, so they answer as native MCP does. A Deployment's answer stays the Store's: the Run card draws it.
    const outcome = write.write.command === "publish" || !gated.result.ok ? yield* finish(agentResume(state, [answered(gated.result)]), read, agentWrite(caller)) : gated.result;
    return done(outcome.ok && gated.nothingDestroyed ? { ...outcome, nothing_destroyed: true } : outcome);
  }
  const acted = yield* runGatedAct(caller, write.write);
  if (acted.linked !== undefined) return done({ ok: true, value: { ...acted.linked, next: `Don't call it again. Call proposal_status with proposal ${acted.linked.proposal_id} to follow it.` } });
  return done(yield* finish(agentResume(state, [answered(acted.result)]), read, agentWrite(caller)));
});
