import "@tanstack/react-start/server-only";
import type { Change, DeploymentStatus, EnvironmentRef, PostedVersion, ServerRef, ServiceVerb } from "@ployz/sdk";
import { and, desc, eq, gt, inArray, isNull, sql } from "drizzle-orm";
import { Effect, Schema } from "effect";
import { Uuid } from "#/lib/schema";
import type { AdmitDeploy } from "#/modules/config-store/admit-deploy";
import { type AgentCaller, type Actor, type Caller, type Credential, callerEnvironments } from "#/modules/identity/actor";
import { askerCaller } from "#/modules/approvals/apply.server";
import { callStore, readStore, writeStoreAsMember } from "#/modules/config-store/config-store.server";
import { protector, protectorOnly } from "#/modules/config-store/protected.server";
import { environmentProtected, findService } from "#/modules/config-store/store-lookup.server";
import { isPending, moved, namesReview, versionService } from "#/modules/deploy-approval/deploy-tap";
import { prepareDeployTap } from "#/modules/deploy-approval/deploy-tap.server";
import { refused, type StoreRefusal } from "#/lib/refusal";
import { oauthCaller } from "#/modules/identity/caller.server";
import { oauthConsent } from "#/modules/identity/oauth-tables";
import { organizationToken, session } from "#/modules/identity/tables";
import { bodyDigest } from "#/modules/operations/keyed.server";
import { OPERATIONS } from "#/modules/operations/operations";
import { actService, unheld } from "#/modules/operations/operations.server";
import { beyondEnvironments, requireAccessById } from "#/modules/organization/access.server";
import { member } from "#/modules/organization/tables";
import {
  DEPLOY_MOVED,
  type DeployStep,
  isDeploy,
  PROPOSAL_STEP_LIMIT,
  type ProposalHalt,
  type ProposalStep,
  type ServiceStep,
  type StepProgress,
  stepService,
} from "#/modules/proposals/proposal";
import { operationProposals } from "#/modules/proposals/tables";
import { tokenRuns } from "#/modules/sandbox/claim-required.server";
import type { ServiceActLeg, ServiceActState } from "#/modules/service-act/service-act";
import { serviceAct } from "#/modules/service-act/tables";
import { Database } from "#/server/database.server";
import { Forbidden, NotFound } from "#/server/public-error";

type ProposalRow = typeof operationProposals.$inferSelect;

type Refused = { readonly ok: false; readonly refusal: StoreRefusal };

type StepAct = {
  readonly proposalStep: number | null;
  readonly state: ServiceActState;
  readonly reason: string | null;
  readonly legs: readonly ServiceActLeg[];
  readonly requestedAt: Date;
};

/** A proposal as the agent and the card read it, with where each step stands given its runs so far. */
function proposalView(row: ProposalRow, acts: readonly StepAct[] = []) {
  return {
    id: row.id,
    status: row.status,
    project: row.project,
    environment: row.environment,
    environment_id: row.environmentId,
    steps: row.steps,
    progress: stepStates(row, acts),
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
  const held = unheld(caller, OPERATIONS["service.restart"].requires, view.environment.name);
  if (held !== null) return held;
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
    callerEnvironments: callerEnvironments(caller) ?? null,
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
  const draft = yield* ownDraft(caller, id);
  if (!draft.ok) return draft;
  const { row } = draft;
  const held = unheld(caller, OPERATIONS[`service.${action.verb}`].requires, row.environment);
  if (held !== null) return held;
  if (row.environmentId !== action.environment) return refused("invalid_argument", `This proposal runs in ${placeOf(row)}. Stage steps for that environment, or create a proposal for this one.`);
  if (row.steps.length >= PROPOSAL_STEP_LIMIT) return refused("invalid_argument", `A proposal holds at most ${PROPOSAL_STEP_LIMIT} steps.`);
  const found = yield* findService(caller.organization.id, action.environment, action.service).pipe(
    Effect.catchTag("StoreRefused", (error) => Effect.succeed(error.refusal)),
  );
  if (found === null) return refused("not_found", `No service is named ${action.service} in ${placeOf(row)}.`);
  if ("code" in found) return { ok: false, refusal: found } satisfies Refused;
  const step: ProposalStep = { verb: action.verb, service: action.service, only: action.only, namespace: found.namespace, runtime: found.runtime };
  const { drizzle } = yield* Database;
  // Appended in the database, so steps staged at once all land, each at its own index.
  const [staged] = yield* drizzle.update(operationProposals)
    .set({ steps: sql`${operationProposals.steps} || ${JSON.stringify(step)}::jsonb`, updatedAt: new Date() })
    .where(and(eq(operationProposals.id, row.id), eq(operationProposals.status, "draft"), sql`jsonb_array_length(${operationProposals.steps}) < ${PROPOSAL_STEP_LIMIT}`))
    .returning({ count: sql<number>`jsonb_array_length(${operationProposals.steps})` });
  if (staged === undefined) {
    const now = yield* readRow(caller.organization.id, id);
    return now?.status === "draft"
      ? refused("invalid_argument", `A proposal holds at most ${PROPOSAL_STEP_LIMIT} steps.`)
      : refused("conflict", "This proposal was already presented. Create a new one to stage more steps.");
  }
  return {
    ok: true as const,
    value: {
      proposal_id: row.id,
      step: staged.count,
      message: `Staged as step ${staged.count}. Nothing ran. Call proposal_present when the plan is complete.`,
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
      message: "Presented. Waiting on a human. Call proposal_status to follow it.",
    },
  };
});

/**
 * A Deploy an agent or a caller without a key asked for, presented to people at once with its whole command as the
 * Store will admit it. A retry of the same Deployment id from the same asker finds the proposal still waiting or running.
 */
export const presentDeployProposal = Effect.fn("Proposals.presentDeploy")(function* (
  caller: Caller,
  place: { readonly id: string; readonly project: string; readonly name: string; readonly namespace: string },
  command: AdmitDeploy,
) {
  const { drizzle } = yield* Database;
  const [again] = yield* drizzle.select({ id: operationProposals.id }).from(operationProposals).where(and(
    eq(operationProposals.organizationId, caller.organization.id),
    eq(operationProposals.requestedByUserId, caller.userId),
    inArray(operationProposals.status, ["presented", "approved"]),
    sql`${operationProposals.steps}->0->'deploy'->'command'->>'id' = ${command.id}`,
  )).limit(1);
  if (again !== undefined) return again;
  const step: DeployStep = { verb: "deploy", deploy: { command } };
  const [row] = yield* drizzle.insert(operationProposals).values({
    organizationId: caller.organization.id,
    environmentId: place.id,
    project: place.project,
    environment: place.name,
    namespace: place.namespace,
    requestedByUserId: caller.userId,
    credentialKind: caller.credential.kind,
    credentialId: caller.credential.id,
    callerEnvironments: callerEnvironments(caller) ?? null,
    steps: [step],
    status: "presented",
  }).returning({ id: operationProposals.id });
  return row ?? (yield* Effect.die("an insert returned no row"));
});

/** Where each step stands: its run's state, waiting for an earlier step, or never run because an earlier one failed. */
const stepStates = (row: ProposalRow, acts: readonly StepAct[]) => {
  const runs = latestRuns(acts);
  let halted = false;
  return row.steps.map((step, index) => {
    const run = runs.get(index);
    const halt = row.halt?.index === index ? row.halt : null;
    const state: StepProgress = run !== undefined ? run.state : halt !== null ? "failed" : halted || row.status !== "approved" ? "not_run" : "waiting";
    const reason = run === undefined ? halt?.reason ?? null : run.reason;
    if (state === "failed" || state === "unknown") halted = true;
    return { index: index + 1, verb: step.verb, service: stepService(step), state, reason };
  });
};

/** Each step's newest run: a step admitted twice, which its request key stops, would still read as its last run. */
const latestRuns = (acts: readonly StepAct[]) =>
  new Map([...acts].sort((left, right) => left.requestedAt.getTime() - right.requestedAt.getTime()).map((act) => [act.proposalStep, act]));

const DEPLOYMENT_STATES = {
  waiting_for_ci: "accepted", queued: "accepted", running: "running", cancelling: "running", applied: "done",
  failed: "failed", cancelled: "failed", superseded: "failed", unknown: "unknown",
} as const satisfies Record<DeploymentStatus, ServiceActState>;

const ENDED = { cancelled: "Someone cancelled it.", superseded: "A newer deploy replaced it." } as const;

/** A Deploy step's run is the Deployment its command names, once the Store admitted it; none before. */
const deployActs = Effect.fn("Proposals.deployActs")(function* (row: ProposalRow) {
  const acts: StepAct[] = [];
  for (const [index, step] of row.steps.entries()) {
    if (!isDeploy(step) || row.status !== "approved") continue;
    const deployment = yield* readStore(row.organizationId, { query: "deployment", id: step.deploy.command.id }).pipe(
      Effect.catchTag("StoreRefused", () => Effect.succeed(null)),
    );
    if (deployment !== null) acts.push({ proposalStep: index, state: DEPLOYMENT_STATES[deployment.status], reason: deployment.status === "cancelled" || deployment.status === "superseded" ? ENDED[deployment.status] : null, legs: [], requestedAt: row.decidedAt ?? row.updatedAt });
  }
  return acts;
});

const actsOf = Effect.fn("Proposals.acts")(function* (row: ProposalRow) {
  const { drizzle } = yield* Database;
  const acts = yield* drizzle.select({ proposalStep: serviceAct.proposalStep, state: serviceAct.state, reason: serviceAct.reason, legs: serviceAct.legs, requestedAt: serviceAct.requestedAt })
    .from(serviceAct).where(and(eq(serviceAct.organizationId, row.organizationId), eq(serviceAct.proposalId, row.id)));
  return [...acts, ...yield* deployActs(row)];
});

/** A proposal and where each of its steps stands, for whoever asks in the Organization. */
export const proposalStatus = Effect.fn("Proposals.status")(function* (caller: Pick<AgentCaller, "organization">, id: string) {
  const row = yield* readRow(caller.organization.id, id);
  if (row === undefined) return refused("not_found", `No proposal is named ${id}.`);
  const { steps: _steps, halt: _halt, progress, ...view } = proposalView(row, yield* actsOf(row));
  return { ok: true as const, value: { ...view, steps: progress } };
});

/** Every proposal waiting on a person in the Organization, newest first. */
export const pendingProposals = Effect.fn("Proposals.pending")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select().from(operationProposals)
    .where(and(eq(operationProposals.organizationId, organizationId), eq(operationProposals.status, "presented")))
    .orderBy(desc(operationProposals.createdAt), desc(operationProposals.id));
  return rows.map((row) => proposalView(row));
});

export const getProposal = Effect.fn("Proposals.get")(function* (organizationId: string, id: string) {
  const row = yield* readRow(organizationId, id);
  if (row === undefined) return yield* new NotFound({ message: "No such proposal." });
  return proposalView(row, yield* actsOf(row));
});

const halted = Effect.fn("Proposals.halted")(function* (row: ProposalRow, index: number, reason: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(operationProposals).set({ halt: { index, reason }, updatedAt: new Date() }).where(eq(operationProposals.id, row.id));
  return { index, reason } satisfies ProposalHalt;
});

/** The request key that admits step `index` once: a second run of the same step replays the first. */
const stepKey = (row: Pick<ProposalRow, "id">, index: number) => `proposal-${row.id}-${index}`;

/** A Deploy that can't go is denied, and the passkey's signed versions are dropped with it: nothing is left to admit later. */
const deniedDeploy = Effect.fn("Proposals.deniedDeploy")(function* (row: ProposalRow, index: number, reason: string) {
  const { drizzle } = yield* Database;
  const cleared = row.steps.map((step): ProposalStep => isDeploy(step) ? { ...step, deploy: { command: { ...step.deploy.command, versions: [] } } } : step);
  yield* drizzle.update(operationProposals).set({ status: "denied", reason: reason.slice(0, 1024), steps: cleared, halt: null, updatedAt: new Date() })
    .where(and(eq(operationProposals.id, row.id), eq(operationProposals.status, "approved")));
  return { index, reason } satisfies ProposalHalt;
});

/** The credential that asked, if it still stands: a token its maker still holds, a grant still usable, a session still signed in. */
const liveCredential = Effect.fn("Proposals.liveCredential")(function* (row: ProposalRow, asker: string) {
  const { drizzle } = yield* Database;
  if (row.credentialKind === "token") {
    const [token] = yield* drizzle.select({ permissions: organizationToken.permissions, environments: organizationToken.environments })
      .from(organizationToken)
      .innerJoin(member, and(eq(member.organizationId, organizationToken.organizationId), eq(member.userId, organizationToken.userId)))
      .where(and(eq(organizationToken.id, row.credentialId), eq(organizationToken.organizationId, row.organizationId), eq(organizationToken.userId, asker), tokenRuns))
      .limit(1);
    return token === undefined ? null : { kind: "token", id: row.credentialId, grant: { permissions: token.permissions, environments: token.environments } } satisfies Credential;
  }
  if (row.credentialKind === "oauth") {
    const [consent] = yield* drizzle.select({ clientId: oauthConsent.clientId }).from(oauthConsent)
      .where(and(eq(oauthConsent.id, row.credentialId), eq(oauthConsent.userId, asker))).limit(1);
    if (consent === undefined) return null;
    const live = yield* oauthCaller({ sub: asker, org: row.organizationId, grant: row.credentialId, azp: consent.clientId }).pipe(
      Effect.catchTag("Unauthorized", () => Effect.succeed(null)),
    );
    return live?.credential ?? null;
  }
  const [live] = yield* drizzle.select({ id: session.id }).from(session)
    .where(and(eq(session.id, row.credentialId), eq(session.userId, asker), gt(session.expiresAt, new Date()))).limit(1);
  return live === undefined ? null : { kind: "session", id: live.id } satisfies Credential;
});

/**
 * Admit a Deploy step's stored command as the asker, with what they hold now, through the credential they asked with.
 * A second run sends the same command, which the Store replays by its Deployment id.
 */
const runDeploy = Effect.fn("Proposals.runDeploy")(function* (row: ProposalRow, step: DeployStep, asker: string) {
  const credential = yield* liveCredential(row, asker);
  if (credential === null) return { refusal: "The sign-in or token that asked for it is gone." };
  const access = yield* requireAccessById({ userId: asker, credential }, row.organizationId, "changes:deploy");
  const environments = credential.kind === "token" && credential.grant.environments !== null ? [...credential.grant.environments] : undefined;
  const beyond = beyondEnvironments(environments, { environment: row.environment });
  if (beyond !== null) return { refusal: beyond };
  const result = yield* callStore(row.organizationId, asker, { operation: "write", command: step.deploy.command }, {
    approval: "not_required", permissions: access.permissions, environments: access.environments, protector: protector(askerCaller(access, row.credentialId)),
  });
  return result.ok ? started : { refusal: result.refusal.message };
});

/** The Service step `index` acts on, or why it can't go to the Servers now, checked before anything is sent. */
const checkedService = Effect.fn("Proposals.checkedService")(function* (row: ProposalRow, step: ServiceStep) {
  const beyond = beyondEnvironments(row.callerEnvironments ?? undefined, { environment: row.environment });
  if (beyond !== null) return { refusal: beyond };
  const found = yield* findService(row.organizationId, row.environmentId, step.service);
  if (found === null) return { refusal: "The service is gone." };
  if (found.namespace !== step.namespace || found.runtime !== step.runtime) return { refusal: `${step.service} changed since it was approved.` };
  return { found };
});

type StepStart = { readonly started: true } | { readonly later: true } | { readonly halt: ProposalHalt };
const started: StepStart = { started: true };
const later: StepStart = { later: true };

/**
 * Start step `index` as the asker. Started once it is admitted,
 * which a rerun replays; halted, recorded on the proposal, when it can't go, which stops the steps after it; later
 * when something failed on Cloud's side, so the sweep tries again rather than give the plan up.
 */
const runStep = Effect.fn("Proposals.runStep")(function* (row: ProposalRow, index: number) {
  const step = row.steps[index];
  if (step === undefined) return started;
  const asker = row.requestedByUserId;
  const halt = (reason: string) => Effect.map(isDeploy(step) ? deniedDeploy(row, index, reason) : halted(row, index, reason), (halt): StepStart => ({ halt }));
  if (asker === null) return yield* halt("Whoever asked is no longer in the Organization.");
  return yield* Effect.gen(function* () {
    if (isDeploy(step)) {
      const ran = yield* runDeploy(row, step, asker);
      return "refusal" in ran ? yield* halt(ran.refusal) : ran;
    }
    const checked = yield* checkedService(row, step);
    if (checked.found === undefined) return yield* halt(checked.refusal);
    const { found } = checked;
    const access = yield* requireAccessById({ userId: asker }, row.organizationId, OPERATIONS[`service.${step.verb}`].requires);
    const body = { environmentId: row.environmentId, service: step.service, only: step.only };
    const admission = yield* actService(
      askerCaller(access, row.credentialId),
      { ...body, ...found, verb: step.verb, digest: bodyDigest(`service_${step.verb}`, body), proposal: { id: row.id, step: index } },
      { key: stepKey(row, index), approvalId: null },
    );
    return admission.ok ? started : yield* halt(admission.refusal.message);
  }).pipe(
    // The asker lost the Organization or the permission: the plan stops. Anything else is Cloud's to retry.
    Effect.catchIf((error) => error instanceof NotFound || error instanceof Forbidden, (error) => halt(error.message)),
    Effect.catchCause(() => Effect.succeed(later)),
  );
});

/**
 * Run the next step of an approved plan that is due: the first step with no run, once every one before it ended done.
 * A step whose run failed or vanished leaves the rest unrun. Safe to call again at any time, from a step's end or the
 * sweep: the step key admits each step once.
 */
const continueProposal = Effect.fn("Proposals.continue")(function* (row: ProposalRow) {
  if (row.status !== "approved" || row.halt !== null) return;
  const runs = latestRuns(yield* actsOf(row));
  const next = row.steps.findIndex((_, index) => !runs.has(index));
  if (next === -1) return;
  const before = next === 0 ? "done" : runs.get(next - 1)?.state;
  if (before === "done") yield* runStep(row, next);
});

/** Once a step's run ends done, start the next; a step that ended any other way leaves the rest unrun. */
export const advanceProposal = Effect.fn("Proposals.advance")(function* (request: { readonly actId: string; readonly organizationId: string }) {
  const { drizzle } = yield* Database;
  const [act] = yield* drizzle.select({ proposalId: serviceAct.proposalId, state: serviceAct.state }).from(serviceAct)
    .where(and(eq(serviceAct.id, request.actId), eq(serviceAct.organizationId, request.organizationId)));
  if (act === undefined || act.proposalId === null || act.state !== "done") return;
  const row = yield* readRow(request.organizationId, act.proposalId);
  if (row !== undefined) yield* continueProposal(row);
});

/** How long after its approval a plan can still have a step to start. */
const SWEEP_WINDOW_MS = 60 * 60_000;

/**
 * Every minute: each approved plan whose next step is due gets it started, so a step lost between its approval and
 * its start, or between one step's end and the next's, still runs.
 */
export const sweepProposals = Effect.fn("Proposals.sweep")(function* (now: Date) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select().from(operationProposals)
    .where(and(eq(operationProposals.status, "approved"), isNull(operationProposals.halt), gt(operationProposals.decidedAt, new Date(now.getTime() - SWEEP_WINDOW_MS))));
  yield* Effect.forEach(rows, continueProposal, { discard: true });
  return rows.map((row) => row.id);
});

/** `versions` are what the decider's passkey signed for a Deploy that Servers run only signed. */
export type ProposalDecision = { readonly approve: { readonly versions?: readonly PostedVersion[] } } | { readonly reject: { readonly reason?: string } };

type Decider = Actor & { readonly organization: { readonly id: string; readonly slug: string }; readonly via?: Caller["via"] };

type Decided = { readonly ok: true; readonly proposal: ProposalView } | { readonly ok: false; readonly message: string };

const settled = (row: ProposalRow): Decided =>
  ({ ok: false, message: row.status === "draft" ? "The agent hasn't presented this proposal yet." : `This proposal was already decided elsewhere (${row.status}).` });

/**
 * A Deploy step as `decider` may approve it: the stored command with the versions their passkey signed, or why not. It
 * re-plans first: a plan that moved denies the proposal, one that needs a passkey answer takes no approval without it,
 * and a Pending value waits for the approval page to fill it.
 */
const approvedDeploy = Effect.fn("Proposals.approvedDeploy")(function* (decider: Decider, row: ProposalRow, step: DeployStep, versions: readonly PostedVersion[]) {
  const { command } = step.deploy;
  const refuse = (message: string) => ({ message }) as const;
  // A person at the approval page decides. An agent, a token or the asker's own `ployz` session never does.
  if ((decider.via ?? "dashboard") !== "dashboard" || (decider.credential !== undefined && decider.credential.kind !== "session")) {
    return refuse("Approve a deploy from its approval page.");
  }
  const access = yield* requireAccessById({ userId: decider.userId }, row.organizationId, "changes:deploy");
  const isProtected = yield* environmentProtected(row.organizationId, row.project, row.environmentId).pipe(Effect.catchTag("StoreRefused", () => Effect.succeed(true)));
  if (isProtected && (access.kind !== "member" || !protector(access))) return refuse("Only an owner or admin can approve a deploy to a Protected Environment.");
  const planned = yield* prepareDeployTap(decider, { organizationSlug: decider.organization.slug, environment: command.environment, services: command.services ?? [], classify: true });
  if (planned.kind === "refused") return refuse(isPending(planned.details) ? "A password has no value yet. Approve this deploy on its approval page." : planned.message);
  if (command.version !== null && command.version !== undefined && !namesReview(planned.plan.version, command.version)) return { moved: true } as const;
  // The asker's own versions still count: the decider signs only the Services those leave open.
  const signed = new Set(versions.map(versionService));
  const kept = (command.versions ?? []).filter((version) => !signed.has(versionService(version)));
  const covered = new Set([...kept, ...versions].map(versionService));
  const open = (planned.plan.unsigned ?? []).filter(({ service }) => !covered.has(service));
  if (open.length > 0 && planned.tap.kind === "refused") return refuse(planned.tap.message);
  if (open.length > 0 && planned.tap.kind === "ready") return refuse("Sign this deploy with your passkey on its approval page.");
  return { command: { ...command, versions: [...kept, ...versions] } satisfies AdmitDeploy };
});

/**
 * Approve every step, which starts the first, or deny with a reason.
 * Only the decision that approved it starts anything, so approving twice never runs a step twice. When the first step
 * doesn't start, the proposal goes back to waiting and the decider hears why; a Deploy is denied instead, with its versions.
 */
export const decideProposal = Effect.fn("Proposals.decide")(function* (decider: Decider, id: string, decision: ProposalDecision) {
  const row = yield* readRow(decider.organization.id, id);
  if (row === undefined) return yield* new NotFound({ message: "No such proposal." });
  if (row.status !== "presented") return settled(row);
  const { drizzle } = yield* Database;
  const now = new Date();
  const decided = { decidedByUserId: decider.userId, decidedAt: now, updatedAt: now };
  const deny = (reason: string | null) => drizzle.update(operationProposals).set({ status: "denied", reason, ...decided })
    .where(and(eq(operationProposals.id, row.id), eq(operationProposals.status, "presented"))).returning();
  if ("reject" in decision) {
    const [denied] = yield* deny(decision.reject.reason?.trim() || null);
    return denied === undefined ? settled((yield* readRow(decider.organization.id, id)) ?? row) : { ok: true, proposal: proposalView(denied) } satisfies Decided;
  }
  const [first] = row.steps;
  let steps = row.steps;
  if (first !== undefined && isDeploy(first)) {
    const checked = yield* approvedDeploy(decider, row, first, decision.approve.versions ?? []);
    if ("moved" in checked) {
      yield* deny(DEPLOY_MOVED);
      return { ok: false, message: DEPLOY_MOVED } satisfies Decided;
    }
    if ("message" in checked) return { ok: false, message: checked.message } satisfies Decided;
    steps = [{ ...first, deploy: { command: checked.command } }];
  }
  const [approved] = yield* drizzle.update(operationProposals).set({ status: "approved", steps, ...decided })
    .where(and(eq(operationProposals.id, row.id), eq(operationProposals.status, "presented"))).returning();
  if (approved === undefined) return settled((yield* readRow(decider.organization.id, id)) ?? row);
  const start = yield* runStep(approved, 0);
  if (!("halt" in start)) return { ok: true, proposal: proposalView(approved, yield* actsOf(approved)) } satisfies Decided;
  // A Deploy that can't go is denied, not left waiting: its signed versions would outlive the plan they signed.
  if (first !== undefined && isDeploy(first)) return { ok: false, message: `It didn't run: ${start.halt.reason}` } satisfies Decided;
  yield* drizzle.update(operationProposals)
    .set({ status: "presented", halt: null, decidedByUserId: null, decidedAt: null, updatedAt: new Date() })
    .where(and(eq(operationProposals.id, row.id), eq(operationProposals.status, "approved")));
  return { ok: false, message: `It didn't run: ${start.halt.reason} It's still waiting for approval.` } satisfies Decided;
});

/**
 * Make a Deploy's Pending values. The approval page sealed each one; they land in one edit at exactly the revision the
 * proposal is bound to, so no other edit slips in, and the proposal is bound one revision on, so a refresh resumes at
 * the signing. An edit made since is the plan moving.
 */
export const fillDeploy = Effect.fn("Proposals.fillDeploy")(function* (decider: Decider, id: string, changes: readonly Change[]) {
  const row = yield* readRow(decider.organization.id, id);
  const [step] = row?.steps ?? [];
  if (row === undefined || row.status !== "presented" || step === undefined || !isDeploy(step)) return { ok: false, message: "This deploy isn't waiting for approval." } as const;
  const { command } = step.deploy;
  const bound = /^(\d+):/u.exec(command.version ?? "")?.[1];
  if (bound === undefined) return { ok: false, message: DEPLOY_MOVED } as const;
  const written = yield* writeStoreAsMember(decider, decider.organization.slug, { command: "edit", environment: command.environment, expect: Number(bound), changes: [...changes] });
  if (!written.ok) return { ok: false, message: written.refusal.code === "conflict" ? DEPLOY_MOVED : written.refusal.message } as const;
  if (written.value.written !== "edited") return { ok: false, message: DEPLOY_MOVED } as const;
  const { revision } = written.value.environment;
  const filled: DeployStep = { ...step, deploy: { command: { ...command, version: moved(command.version, revision) } } };
  const { drizzle } = yield* Database;
  const [saved] = yield* drizzle.update(operationProposals).set({ steps: [filled], updatedAt: new Date() }).where(and(
    eq(operationProposals.id, row.id), eq(operationProposals.status, "presented"),
    sql`${operationProposals.steps}->0->'deploy'->'command'->>'version' = ${command.version ?? ""}`,
  )).returning({ id: operationProposals.id });
  return saved === undefined ? { ok: false, message: DEPLOY_MOVED } as const : { ok: true, revision } as const;
});
