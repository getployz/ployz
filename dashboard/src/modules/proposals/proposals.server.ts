import "@tanstack/react-start/server-only";
import type { EnvironmentRef, ServerRef, ServiceVerb, Signed } from "@ployz/sdk";
import { and, desc, eq } from "drizzle-orm";
import { Cause, Effect, Schema } from "effect";
import { toErrorMessage } from "#/lib/error-message";
import { Uuid } from "#/lib/schema";
import type { AgentCaller } from "#/modules/agent/agent-runner.server";
import { askerCaller } from "#/modules/approvals/apply.server";
import { readStore } from "#/modules/config-store/config-store.server";
import { protectorOnly } from "#/modules/config-store/protected.server";
import { environmentProtected, findService } from "#/modules/config-store/store-lookup.server";
import type { StoreRefusal } from "#/modules/config-store/store.contract";
import type { Actor } from "#/modules/identity/actor";
import { bodyDigest } from "#/modules/operations/keyed.server";
import { OPERATIONS } from "#/modules/operations/operations";
import { actService, unpermitted } from "#/modules/operations/operations.server";
import { requireAccessById } from "#/modules/organization/access.server";
import {
  PROPOSAL_STEP_LIMIT,
  type ProposalApprovals,
  type ProposalHalt,
  type ProposalStep,
  strandedByProposal,
} from "#/modules/proposals/proposal";
import { operationProposals } from "#/modules/proposals/tables";
import { needsNewerPloyz, type ServiceActState, strandedByStop } from "#/modules/service-act/service-act";
import { actTarget, strandingStop } from "#/modules/service-act/service-act.server";
import { serviceAct } from "#/modules/service-act/tables";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";

type ProposalRow = typeof operationProposals.$inferSelect;

type Refused = { readonly ok: false; readonly refusal: StoreRefusal };
const refused = (code: string, message: string): Refused => ({ ok: false, refusal: { code, message, details: null } });

/** A proposal as the agent and the card read it. */
function proposalView(row: ProposalRow) {
  return {
    id: row.id,
    status: row.status,
    project: row.project,
    environment: row.environment,
    environment_id: row.environmentId,
    steps: row.steps,
    halt: row.halt,
    reason: row.reason,
    requested_by: row.requestedByUserId,
    created_at: row.createdAt.toISOString(),
    decided_at: row.decidedAt?.toISOString() ?? null,
  };
}
export type ProposalView = ReturnType<typeof proposalView>;

const readRow = Effect.fn("Proposals.readRow")(function* (organizationId: string, id: string) {
  if (!Schema.is(Uuid)(id)) return undefined;
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select().from(operationProposals)
    .where(and(eq(operationProposals.id, id), eq(operationProposals.organizationId, organizationId)));
  return row;
});

const placeOf = (row: Pick<ProposalRow, "project" | "environment">) => `${row.project}/${row.environment}`;

/** The asker's own draft, or why `caller` may not add to `id`. */
const ownDraft = Effect.fn("Proposals.ownDraft")(function* (caller: AgentCaller, id: string) {
  const row = yield* readRow(caller.organization.id, id);
  if (row === undefined) return refused("not_found", `No proposal is named ${id}.`);
  const theirs = row.requestedByUserId === caller.userId && row.credentialKind === caller.credential.kind && row.credentialId === caller.credential.id;
  if (!theirs) return refused("forbidden", "This proposal was staged by someone else. Create your own.");
  if (row.status !== "draft") return refused("conflict", `This proposal was already ${row.status}. Create a new one to stage more steps.`);
  return { ok: true as const, row };
});

/** Start an empty proposal in the Environment `place` names, defaulting to the Organization's. */
export const createProposal = Effect.fn("Proposals.create")(function* (caller: AgentCaller, place: EnvironmentRef) {
  const view = yield* readStore(caller.organization.id, { query: "namespace", environment: place }).pipe(
    Effect.catchTag("StoreRefused", (error) => Effect.succeed(error.refusal)),
  );
  if ("code" in view) return { ok: false, refusal: view } satisfies Refused;
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.insert(operationProposals).values({
    organizationId: caller.organization.id,
    environmentId: view.environment.id,
    project: view.environment.project,
    environment: view.environment.name,
    namespace: view.namespace,
    requestedByUserId: caller.userId,
    credentialKind: caller.credential.kind,
    credentialId: caller.credential.id,
  }).returning();
  if (row === undefined) return yield* Effect.die("an insert returned no row");
  return {
    ok: true as const,
    value: {
      proposal_id: row.id,
      environment: placeOf(row),
      message: `Proposal ${row.id} is open in ${placeOf(row)}. Stage each step with proposal set to it, in the order to run, then call proposal_present.`,
    },
  };
});

/** Add a restart, stop or start to the asker's draft, as the Servers know the Service now. */
export const stageStep = Effect.fn("Proposals.stage")(function* (
  caller: AgentCaller,
  id: string,
  action: { readonly environment: string; readonly service: string; readonly verb: ServiceVerb; readonly only: ServerRef | null },
) {
  const lacking = unpermitted(caller, OPERATIONS[`service.${action.verb}`].requires);
  if (lacking !== null) return lacking;
  const draft = yield* ownDraft(caller, id);
  if (!draft.ok) return draft;
  const { row } = draft;
  if (row.environmentId !== action.environment) return refused("invalid_argument", `This proposal runs in ${placeOf(row)}. Stage steps for that environment, or create a proposal for this one.`);
  if (row.steps.length >= PROPOSAL_STEP_LIMIT) return refused("invalid_argument", `A proposal holds at most ${PROPOSAL_STEP_LIMIT} steps.`);
  const found = yield* findService(caller.organization.id, action.environment, action.service).pipe(
    Effect.catchTag("StoreRefused", (error) => Effect.succeed(error.refusal)),
  );
  if (found === null) return refused("not_found", `No service is named ${action.service} in ${placeOf(row)}.`);
  if ("code" in found) return { ok: false, refusal: found } satisfies Refused;
  const step: ProposalStep = { verb: action.verb, service: action.service, only: action.only, namespace: found.namespace, runtime: found.runtime };
  const steps = [...row.steps, step];
  const { drizzle } = yield* Database;
  const [staged] = yield* drizzle.update(operationProposals).set({ steps, updatedAt: new Date() })
    .where(and(eq(operationProposals.id, row.id), eq(operationProposals.status, "draft"))).returning({ id: operationProposals.id });
  if (staged === undefined) return refused("conflict", "This proposal was already presented. Create a new one to stage more steps.");
  return {
    ok: true as const,
    value: {
      proposal_id: row.id,
      step: steps.length,
      message: `Staged as step ${steps.length}. Nothing ran. Call proposal_present when the plan is complete.`,
    },
  };
});

/** Hand the asker's draft to the people who can approve it. */
export const presentProposal = Effect.fn("Proposals.present")(function* (caller: AgentCaller, id: string) {
  const draft = yield* ownDraft(caller, id);
  if (!draft.ok) return draft;
  const { row } = draft;
  if (row.steps.length === 0) return refused("invalid_argument", "This proposal has no steps. Stage one first.");
  // Each step runs as the asker once approved, so a Protected Environment takes a proposal only from someone who can protect it.
  if (row.steps.some((step) => step.verb !== "start")) {
    const isProtected = yield* environmentProtected(caller.organization.id, row.project, row.environmentId).pipe(
      Effect.catchTag("StoreRefused", () => Effect.succeed(true)),
    );
    const guarded = isProtected ? protectorOnly(caller, row.environment) : null;
    if (guarded !== null) return { ok: false, refusal: guarded } satisfies Refused;
  }
  const { drizzle } = yield* Database;
  const [presented] = yield* drizzle.update(operationProposals).set({ status: "presented", updatedAt: new Date() })
    .where(and(eq(operationProposals.id, row.id), eq(operationProposals.status, "draft"))).returning({ id: operationProposals.id });
  if (presented === undefined) return refused("conflict", "This proposal was already presented.");
  return {
    ok: true as const,
    value: {
      proposal_id: row.id,
      steps: row.steps.length,
      message: "Presented. Waiting on a person. Call proposal_status to follow it.",
    },
  };
});

type StepState = ServiceActState | "waiting" | "not_run";

/** Where each step stands: its run's state, waiting for an earlier step, or never run because an earlier one failed. */
const stepStates = (row: ProposalRow, acts: readonly { proposalStep: number | null; state: ServiceActState; reason: string | null }[]) => {
  const runs = new Map(acts.map((act) => [act.proposalStep, act]));
  let halted = false;
  return row.steps.map((step, index) => {
    const run = runs.get(index);
    const halt = row.halt?.index === index ? row.halt : null;
    const state: StepState = run !== undefined ? run.state : halt !== null ? "failed" : halted || row.status !== "approved" ? "not_run" : "waiting";
    const reason = run?.reason ?? halt?.reason ?? null;
    if (state === "failed" || state === "unknown") halted = true;
    return { index: index + 1, verb: step.verb, service: step.service, state, reason };
  });
};

const actsOf = Effect.fn("Proposals.acts")(function* (row: ProposalRow) {
  const { drizzle } = yield* Database;
  return yield* drizzle.select({ proposalStep: serviceAct.proposalStep, state: serviceAct.state, reason: serviceAct.reason })
    .from(serviceAct).where(and(eq(serviceAct.organizationId, row.organizationId), eq(serviceAct.proposalId, row.id)));
});

/** A proposal and where each of its steps stands, for whoever asks in the Organization. */
export const proposalStatus = Effect.fn("Proposals.status")(function* (caller: Pick<AgentCaller, "organization">, id: string) {
  const row = yield* readRow(caller.organization.id, id);
  if (row === undefined) return refused("not_found", `No proposal is named ${id}.`);
  const acts = yield* actsOf(row);
  const { steps: _steps, halt: _halt, ...view } = proposalView(row);
  return { ok: true as const, value: { ...view, steps: stepStates(row, acts) } };
});

/** Every proposal waiting on a person in the Organization, newest first. */
export const pendingProposals = Effect.fn("Proposals.pending")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select().from(operationProposals)
    .where(and(eq(operationProposals.organizationId, organizationId), eq(operationProposals.status, "presented")))
    .orderBy(desc(operationProposals.createdAt), desc(operationProposals.id));
  return rows.map(proposalView);
});

export const getProposal = Effect.fn("Proposals.get")(function* (organizationId: string, id: string) {
  const row = yield* readRow(organizationId, id);
  if (row === undefined) return yield* new NotFound({ message: "No such proposal." });
  return proposalView(row);
});

/**
 * What the approver's browser signs for each step: the Service as the Servers know it and its Containers, with the
 * Servers and which of them run the Environment. Null when Cloud can't read the Servers.
 */
export const proposalTarget = Effect.fn("Proposals.target")(function* (organizationId: string, row: ProposalRow) {
  const steps = [];
  let servers = null;
  for (const step of row.steps) {
    const target = yield* actTarget(organizationId, step);
    if (target === null) return null;
    servers = target;
    const copies = target.copies.filter((copy) => step.only === null || copy.machineId === step.only.machine);
    steps.push({ verb: step.verb, service: step.service, namespace: step.namespace, runtime: step.runtime, copies });
  }
  if (servers === null) return null;
  return { environmentId: row.environmentId, environmentName: row.environment, namespace: row.namespace, steps, machines: servers.machines, environmentMachines: servers.environmentMachines };
});

export const readProposalTarget = Effect.fn("Proposals.readTarget")(function* (organizationId: string, id: string) {
  const row = yield* readRow(organizationId, id);
  return row === undefined ? null : yield* proposalTarget(organizationId, row);
});

/** Why a step couldn't go to the Servers as the proposal stands, or null when every step could. Refuses rather than guess when Cloud can't read them. */
const stranding = Effect.fn("Proposals.stranding")(function* (row: ProposalRow) {
  const target = yield* proposalTarget(row.organizationId, row);
  if (target === null) return "Cloud couldn't check the servers. Try again.";
  for (const step of target.steps) {
    const older = strandedByProposal(step.copies, target.machines);
    if (older !== null) return needsNewerPloyz(older, step.verb);
    const stranded = step.verb === "stop" ? strandedByStop(step.copies, target.machines) : null;
    if (stranded !== null) return needsNewerPloyz(stranded, "stop");
  }
  return null;
});

const halted = Effect.fn("Proposals.halted")(function* (row: ProposalRow, index: number, reason: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(operationProposals).set({ halt: { index, reason }, updatedAt: new Date() }).where(eq(operationProposals.id, row.id));
  return { index, reason } satisfies ProposalHalt;
});

/** The request key that admits step `index` once: a second run of the same step replays the first. */
const stepKey = (row: Pick<ProposalRow, "id">, index: number) => `proposal-${row.id}-${index}`;

/**
 * Start step `index` as the asker, with the envelopes the approver's browser made for it. Null once it is admitted;
 * otherwise why it wasn't, recorded on the proposal, which stops the steps after it.
 */
const runStep = Effect.fn("Proposals.runStep")(function* (row: ProposalRow, index: number) {
  const step = row.steps[index];
  if (step === undefined) return null;
  const asker = row.requestedByUserId;
  if (asker === null) return yield* halted(row, index, "Whoever asked is no longer in the Organization.");
  const outcome: ProposalHalt | null = yield* Effect.gen(function* () {
    const access = yield* requireAccessById({ userId: asker }, row.organizationId, OPERATIONS[`service.${step.verb}`].requires);
    const found = yield* findService(row.organizationId, row.environmentId, step.service);
    if (found === null) return { index, reason: "The service is gone." };
    if (found.namespace !== step.namespace || found.runtime !== step.runtime) return { index, reason: `${step.service} changed since it was approved.` };
    const stranded = step.verb === "stop" ? yield* strandingStop(row.organizationId, found, true) : null;
    if (stranded !== null) return { index, reason: stranded };
    const body = { environmentId: row.environmentId, service: step.service, only: step.only };
    const admission = yield* actService(
      askerCaller(access, row.credentialId),
      { ...body, ...found, verb: step.verb, digest: bodyDigest(`service_${step.verb}`, body), approvals: row.approvals?.[index] ?? {}, proposal: { id: row.id, step: index } },
      { key: stepKey(row, index), approvalId: null },
    );
    return admission.ok ? null : { index, reason: admission.refusal.message };
  }).pipe(Effect.catchCause((cause) => Effect.succeed({ index, reason: toErrorMessage(Cause.squash(cause), "Something went wrong.") })));
  return outcome === null ? null : yield* halted(row, index, outcome.reason);
});

/** Once a step's run ends done, start the next; a step that ended any other way leaves the rest unrun. */
export const advanceProposal = Effect.fn("Proposals.advance")(function* (request: { readonly actId: string; readonly organizationId: string }) {
  const { drizzle } = yield* Database;
  const [act] = yield* drizzle.select({ proposalId: serviceAct.proposalId, step: serviceAct.proposalStep, state: serviceAct.state }).from(serviceAct)
    .where(and(eq(serviceAct.id, request.actId), eq(serviceAct.organizationId, request.organizationId)));
  if (act === undefined || act.proposalId === null || act.step === null || act.state !== "done") return;
  const row = yield* readRow(request.organizationId, act.proposalId);
  if (row === undefined || row.status !== "approved" || row.halt !== null) return;
  yield* runStep(row, act.step + 1);
});

export type ProposalDecision = { readonly approve: { readonly approvals: ProposalApprovals; readonly bundle: Signed } } | { readonly reject: { readonly reason?: string } };

type Decided = { readonly ok: true; readonly proposal: ProposalView } | { readonly ok: false; readonly message: string };

const settled = (row: ProposalRow): Decided =>
  ({ ok: false, message: row.status === "draft" ? "The agent hasn't presented this proposal yet." : `This proposal was already decided elsewhere (${row.status}).` });

/**
 * Approve every step with the envelopes the approver's browser made, which starts the first, or deny with a reason.
 * Only the decision that approved it starts anything, so approving twice never runs a step twice. When the first step
 * doesn't start, the proposal goes back to waiting and the decider hears why.
 */
export const decideProposal = Effect.fn("Proposals.decide")(function* (decider: Actor & { readonly organization: { readonly id: string } }, id: string, decision: ProposalDecision) {
  const row = yield* readRow(decider.organization.id, id);
  if (row === undefined) return yield* new NotFound({ message: "No such proposal." });
  if (row.status !== "presented") return settled(row);
  const { drizzle } = yield* Database;
  const now = new Date();
  const decided = { decidedByUserId: decider.userId, decidedAt: now, updatedAt: now };
  if ("reject" in decision) {
    const [denied] = yield* drizzle.update(operationProposals).set({ status: "denied", reason: decision.reject.reason ?? null, ...decided })
      .where(and(eq(operationProposals.id, row.id), eq(operationProposals.status, "presented"))).returning();
    return denied === undefined ? settled((yield* readRow(decider.organization.id, id)) ?? row) : { ok: true, proposal: proposalView(denied) } satisfies Decided;
  }
  const { approvals, bundle } = decision.approve;
  if (row.steps.some((_, index) => approvals[String(index)] === undefined)) return { ok: false, message: "The approval doesn't cover every step. Nothing was approved." } satisfies Decided;
  const stranded = yield* stranding(row);
  if (stranded !== null) return { ok: false, message: stranded } satisfies Decided;
  const [approved] = yield* drizzle.update(operationProposals).set({ status: "approved", bundle, approvals, ...decided })
    .where(and(eq(operationProposals.id, row.id), eq(operationProposals.status, "presented"))).returning();
  if (approved === undefined) return settled((yield* readRow(decider.organization.id, id)) ?? row);
  const halt = yield* runStep(approved, 0);
  if (halt === null) return { ok: true, proposal: proposalView(approved) } satisfies Decided;
  yield* drizzle.update(operationProposals)
    .set({ status: "presented", bundle: null, approvals: null, halt: null, decidedByUserId: null, decidedAt: null, updatedAt: new Date() })
    .where(and(eq(operationProposals.id, row.id), eq(operationProposals.status, "approved")));
  return { ok: false, message: `It didn't run: ${halt.reason} It's still waiting for approval.` } satisfies Decided;
});
