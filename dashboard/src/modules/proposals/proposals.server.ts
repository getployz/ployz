import "@tanstack/react-start/server-only";
import { createHash } from "node:crypto";
import type { Change, ConfigTrusted, DeploymentStatus, DestructiveEffect, DiffView, EnvironmentRef, PostedVersion, ServerRef, ServiceVerb } from "@ployz/sdk";
import { and, desc, eq, gt, inArray, isNull, sql } from "drizzle-orm";
import { Effect, Option, Schema } from "effect";
import { canonicalJson, projectJsonValue } from "#/lib/json";
import { Uuid } from "#/lib/schema";
import type { AdmitDeploy } from "#/modules/config-store/admit-deploy";
import { type AgentCaller, type Actor, type Caller, type Credential, callerEnvironments } from "#/modules/identity/actor";
import { callStore, readStore, writeStoreAsMember } from "#/modules/config-store/config-store.server";
import { protector } from "#/modules/config-store/protected.server";
import { OWNER_OR_ADMIN } from "#/modules/operations/gate.server";
import { withoutSealedBefore } from "#/modules/config-store/store-diff";
import { environmentProtected, findService } from "#/modules/config-store/store-lookup.server";
import { isPending, moved, namesReview, versionService } from "#/modules/deploy-approval/deploy-tap";
import { prepareDeployTap } from "#/modules/deploy-approval/deploy-tap.server";
import { refusal, refused, type StoreRefusal } from "#/lib/refusal";
import { oauthCaller } from "#/modules/identity/caller.server";
import { oauthConsent } from "#/modules/identity/oauth-tables";
import { organizationToken, session, user } from "#/modules/identity/tables";
import { bodyDigest } from "#/modules/operations/keyed.server";
import { OPERATIONS } from "#/modules/operations/operations";
import { actService, unheld } from "#/modules/operations/operations.server";
import { type Access, beyondEnvironments, requireAccessById } from "#/modules/organization/access.server";
import { member } from "#/modules/organization/tables";
import {
  DEPLOY_MOVED,
  type DeployReview,
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
import { checkedNow } from "#/db/tables";

type ProposalRow = typeof operationProposals.$inferSelect;

/** The member who asked, acting from the dashboard with what they hold now, through the credential that asked. */
export const askerCaller = (access: Access, credentialId: string): Caller => ({
  userId: access.userId,
  organization: access.organization,
  credential: { kind: "session", id: credentialId },
  role: access.kind === "member" ? access.role : null,
  support: access.kind === "support" ? access.shows : null,
  via: "dashboard",
});

/** The names of the people who asked, keyed by their user id, so a card says whom it waits for. */
export const askerNames = Effect.fn("Proposals.askerNames")(function* (asked: readonly { readonly requested_by: string | null }[]) {
  const ids = [...new Set(asked.flatMap((row) => row.requested_by ?? []))];
  if (ids.length === 0) return new Map<string, string>();
  const { drizzle } = yield* Database;
  const people = yield* drizzle.select({ id: user.id, name: user.name }).from(user).where(inArray(user.id, ids));
  return new Map(people.map((person) => [person.id, person.name]));
});

const isEffect = Schema.is(Schema.Struct({ kind: Schema.String, node: Schema.String, path: Schema.String }));
const isDiff = Schema.is(Schema.Struct({
  version: Schema.String,
  environment: Schema.Struct({ id: Schema.String, project: Schema.String, name: Schema.String }),
}));

/** The review an `approval_required` refusal carries: the Store's `DestructiveEffect[]` and `DiffView`, by the fields Cloud reads. */
export const RefusedReview = Schema.Struct({
  approval: Schema.String,
  effects: Schema.mutable(Schema.Array(Schema.declare<DestructiveEffect>((value): value is DestructiveEffect => isEffect(value)))),
  diff: Schema.declare<DiffView>((value): value is DiffView => isDiff(value)),
  protected: Schema.optional(Schema.Literal(true)),
  redeploy: Schema.optional(Schema.Struct({ deployment: Schema.Number, rolls_back: Schema.mutable(Schema.Array(Schema.Number)) })),
  prunes: Schema.optional(Schema.Literal(true)),
  dropped: Schema.optional(Schema.mutable(Schema.Array(Schema.String))),
});

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
    withdrawn: row.withdrawn,
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
    .set({ steps: sql`${operationProposals.steps} || ${JSON.stringify(step)}::jsonb`, updatedAt: sql`now()` })
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
  const { drizzle } = yield* Database;
  const [presented] = yield* drizzle.update(operationProposals).set({ status: "presented", updatedAt: sql`now()` })
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

/** A digest of a Deploy as it was asked, which tells a retry of it from another Deploy under the same Deployment id. */
const askedDigest = (command: AdmitDeploy) => createHash("sha256").update(canonicalJson(projectJsonValue(command) ?? null)).digest("hex");

/**
 * The proposal still waiting or approved under the Deployment id `asked` names: this person's same request finds it
 * again, and any other body under that id is refused. Null when there is none.
 */
export const askedDeployProposal = Effect.fn("Proposals.askedDeploy")(function* (caller: Caller, asked: AdmitDeploy) {
  const { drizzle } = yield* Database;
  const [again] = yield* drizzle.select({ id: operationProposals.id, status: operationProposals.status, by: operationProposals.requestedByUserId, steps: operationProposals.steps }).from(operationProposals).where(and(
    eq(operationProposals.organizationId, caller.organization.id),
    inArray(operationProposals.status, ["presented", "approved"]),
    sql`${operationProposals.steps}->0->'deploy'->'command'->>'id' = ${asked.id}`,
  )).limit(1);
  if (again === undefined) return null;
  const [step] = again.steps;
  const same = again.by === caller.userId && step !== undefined && isDeploy(step) && step.deploy.asked === askedDigest(asked);
  if (!same) return { refused: refusal("conflict", "A different deploy already asked for approval under this ID. Nothing deployed.") };
  return { id: again.id, status: again.status === "approved" ? "approved" as const : "presented" as const };
});

/**
 * A Deploy an agent or a caller without a key asked for, presented to people at once with its whole command as the
 * Store will admit it and the review it was asked against. The same request asked again by the same person finds the
 * proposal still waiting or running, under a lock so two retries at once make one. The Deployment id asked with
 * another body is refused.
 */
export const presentDeployProposal = Effect.fn("Proposals.presentDeploy")(function* (
  caller: Caller,
  place: { readonly id: string; readonly project: string; readonly name: string; readonly namespace: string },
  deploy: { readonly asked: AdmitDeploy; readonly bound: AdmitDeploy; readonly review: DeployReview | undefined },
) {
  const asked = askedDigest(deploy.asked);
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`select pg_advisory_xact_lock(hashtext(${`deploy-proposal:${caller.organization.id}:${deploy.asked.id}`}))`);
    const again = yield* askedDeployProposal(caller, deploy.asked);
    if (again !== null) return again;
    const step: DeployStep = { verb: "deploy", deploy: deploy.review === undefined ? { command: deploy.bound, asked } : { command: deploy.bound, asked, review: deploy.review } };
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
    return row === undefined ? yield* Effect.die("an insert returned no row") : { id: row.id, status: "presented" as const };
  }));
});

/** What the Store weighs about who asks a Deploy: what they hold, and whether they protect its Environment. */
export type AskerGate = Pick<ConfigTrusted, "permissions" | "environments" | "protector">;

const LossAsked = Schema.Struct({ accept: Schema.Array(Schema.String), version: Schema.String });

/**
 * What the Store asks a person to approve before it admits `command` from the asker `gate` describes: the command bound
 * to the Volume loss it names, with the review and the digest admission redeems. Nothing runs: asking at every Deploy,
 * the Store refuses before it admits. Null only when it admitted anyway.
 */
export const reviewDeploy = Effect.fn("Proposals.reviewDeploy")(function* (organizationId: string, userId: string, gate: AskerGate, command: AdmitDeploy) {
  const probe = (bound: AdmitDeploy) => callStore(organizationId, userId, { operation: "write", command: bound }, { ...gate, approval: "required", asks: "every_deploy" });
  let bound = command;
  let asked = yield* probe(bound);
  const loss = !asked.ok && asked.refusal.code === "confirmation_required" ? Schema.decodeUnknownOption(LossAsked)(asked.refusal.details) : Option.none();
  if (Option.isSome(loss)) {
    bound = { ...bound, version: loss.value.version, accept_volume_loss: [...loss.value.accept] };
    asked = yield* probe(bound);
  }
  if (asked.ok) return null;
  const reviewed = asked.refusal.code === "approval_required" ? Schema.decodeUnknownOption(RefusedReview)(asked.refusal.details) : Option.none();
  if (Option.isNone(reviewed)) return { refused: asked.refusal };
  const { approval: digest, effects, diff, prunes, dropped } = reviewed.value;
  const review: DeployReview["review"] = { effects, diff: withoutSealedBefore(diff) };
  if (prunes) review.prunes = prunes;
  if (prunes && dropped !== undefined && dropped.length > 0) review.dropped = dropped;
  if (reviewed.value.protected) review.protected = { environment: diff.environment.name };
  return { command: bound, review: { digest, review } satisfies DeployReview };
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

/** Where a Deploy the CLI asked for stands, for the credential that asked and no other: its status, why it was denied, and the Deployment it admits. */
export const askedDeploy = Effect.fn("Proposals.askedDeploy")(function* (caller: Caller, id: string) {
  if (!Schema.is(Uuid)(id)) return undefined;
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ status: operationProposals.status, reason: operationProposals.reason, steps: operationProposals.steps })
    .from(operationProposals)
    .where(and(
      eq(operationProposals.id, id),
      eq(operationProposals.organizationId, caller.organization.id),
      eq(operationProposals.requestedByUserId, caller.userId),
      eq(operationProposals.credentialKind, caller.credential.kind),
      eq(operationProposals.credentialId, caller.credential.id),
    ));
  const deploy = row?.steps.find(isDeploy);
  return row === undefined || deploy === undefined ? undefined : { status: row.status, reason: row.reason, deployment_id: deploy.deploy.command.id };
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
  yield* drizzle.update(operationProposals).set({ halt: { index, reason }, updatedAt: sql`now()` }).where(eq(operationProposals.id, row.id));
  return { index, reason } satisfies ProposalHalt;
});

/** The request key that admits step `index` once: a second run of the same step replays the first. */
const stepKey = (row: Pick<ProposalRow, "id">, index: number) => `proposal-${row.id}-${index}`;

/** `command` without an automatic deploy rule, which no approval page shows, so no proposal admits one. */
const ruleless = ({ rule: _rule, ...command }: AdmitDeploy): AdmitDeploy => command;

/** A Deploy that can't go is denied, and the passkey's signed versions are dropped with it: nothing is left to admit later. */
const deniedDeploy = Effect.fn("Proposals.deniedDeploy")(function* (row: ProposalRow, index: number, reason: string) {
  const { drizzle } = yield* Database;
  const cleared = row.steps.map((step): ProposalStep => isDeploy(step) ? { ...step, deploy: { ...step.deploy, command: { ...ruleless(step.deploy.command), versions: [] } } } : step);
  yield* drizzle.update(operationProposals).set({ status: "denied", reason: reason.slice(0, 1024), steps: cleared, halt: null, updatedAt: sql`now()` })
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
    .where(and(eq(session.id, row.credentialId), eq(session.userId, asker), gt(session.expiresAt, checkedNow()))).limit(1);
  return live === undefined ? null : { kind: "session", id: live.id } satisfies Credential;
});

/** The asker as the Store weighs them now, through the credential they asked with, or why they can't ask any more. */
const askerNow = Effect.fn("Proposals.askerNow")(function* (row: ProposalRow, asker: string) {
  const credential = yield* liveCredential(row, asker);
  if (credential === null) return { refusal: "The sign-in or token that asked for it is gone." };
  const access = yield* requireAccessById({ userId: asker, credential }, row.organizationId, "changes:deploy");
  const environments = credential.kind === "token" && credential.grant.environments !== null ? [...credential.grant.environments] : undefined;
  const beyond = beyondEnvironments(environments, { environment: row.environment });
  if (beyond !== null) return { refusal: beyond };
  return { gate: { permissions: access.permissions, environments: access.environments, protector: protector(askerCaller(access, row.credentialId)) } satisfies AskerGate };
});

/**
 * Admit a Deploy step's stored command as the asker, with what they hold now, through the credential they asked with,
 * redeeming the review the person approved as the Store asked it, at every Deploy: a Deploy that would destroy
 * anything else by now, or no longer destroys what was shown, is refused, not run. A
 * second run sends the same command, which the Store replays by its Deployment id.
 */
const runDeploy = Effect.fn("Proposals.runDeploy")(function* (row: ProposalRow, step: DeployStep, asker: string) {
  const now = yield* askerNow(row, asker);
  if (now.refusal !== undefined) return { refusal: now.refusal };
  const { command, review } = step.deploy;
  const result = yield* callStore(row.organizationId, asker, { operation: "write", command }, review === undefined ? { ...now.gate, approval: "required" } : { ...now.gate, approval: { approved: review.digest }, asks: "every_deploy" });
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

/** Whether `userId` is an owner or admin of the Organization now, who may decide in a Protected Environment. */
const protectorNow = Effect.fn("Proposals.protectorNow")(function* (userId: string | null, organizationId: string) {
  if (userId === null) return false;
  return yield* requireAccessById({ userId }, organizationId, "changes:deploy").pipe(
    Effect.map((access) => access.kind === "member" && protector(access)),
    Effect.catchIf((error) => error instanceof NotFound || error instanceof Forbidden, () => Effect.succeed(false)),
  );
});

/** Whether `userId` may decide `row` as its Environment stands now: anyone outside a Protected one, else an owner or admin. */
const protects = Effect.fn("Proposals.protects")(function* (userId: string | null, row: ProposalRow) {
  const isProtected = yield* environmentProtected(row.organizationId, row.project, row.environmentId).pipe(Effect.catchTag("StoreRefused", () => Effect.succeed(true)));
  return !isProtected || (yield* protectorNow(userId, row.organizationId));
});

/** Whether `steps` hold a Deploy, stop or restart, which a Protected Environment leaves to an owner or admin. */
const guarded = (steps: readonly ProposalStep[]) => steps.some((step) => isDeploy(step) || step.verb !== "start");

/** Whether `userId` may approve `row`: anyone who answers, unless a Deploy, stop or restart waits in a Protected Environment. */
const decides = Effect.fn("Proposals.decides")(function* (userId: string | null, row: ProposalRow) {
  return !guarded(row.steps) || (yield* protects(userId, row));
});

/** The Environments of `project` that are Protected now, by id; null when the Store can't say, which reads as every one. */
const protectedIn = Effect.fn("Proposals.protectedIn")(function* (organizationId: string, project: string) {
  return yield* readStore(organizationId, { query: "environments", project }).pipe(
    Effect.map(({ environments }) => new Set(environments.flatMap(({ id, protected: isProtected }) => isProtected ? [id] : []))),
    Effect.catchTag("StoreRefused", () => Effect.succeed(null)),
  );
});

/**
 * `views` as `userId` sees them: which they asked for, and which they may approve. The sidebar polls this, so it reads
 * each Project's Environments once and the viewer once, however many proposals wait. Deciding checks again, live.
 */
export const asSeenBy = Effect.fn("Proposals.asSeenBy")(function* (organizationId: string, userId: string, views: readonly ProposalView[]) {
  const projects = [...new Set(views.filter((view) => guarded(view.steps)).map((view) => view.project))];
  const held = new Map(yield* Effect.forEach(projects, (project) => Effect.map(protectedIn(organizationId, project), (ids) => [project, ids] as const)));
  const isProtected = (view: ProposalView) => {
    const ids = held.get(view.project);
    return ids === undefined || ids === null || ids.has(view.environment_id);
  };
  const asksProtector = views.some((view) => guarded(view.steps) && isProtected(view));
  const protects = asksProtector && (yield* protectorNow(userId, organizationId));
  return views.map((view) => ({ ...view, mine: view.requested_by === userId, decides: !guarded(view.steps) || !isProtected(view) || protects }));
});

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
    // An Environment protected since the approval takes an owner's or admin's: a stop or restart runs only while whoever approved it still could.
    if (step.verb !== "start" && !(yield* protects(row.decidedByUserId, row))) return yield* halt(`${row.environment} is Protected now. An owner or admin must approve it.`);
    const checked = yield* checkedService(row, step);
    if (checked.found === undefined) return yield* halt(checked.refusal);
    const { found } = checked;
    const access = yield* requireAccessById({ userId: asker }, row.organizationId, OPERATIONS[`service.${step.verb}`].requires);
    const body = { environmentId: row.environmentId, service: step.service, only: step.only };
    const admission = yield* actService(
      askerCaller(access, row.credentialId),
      { ...body, ...found, verb: step.verb, digest: bodyDigest(`service_${step.verb}`, body), proposal: { id: row.id, step: index } },
      { key: stepKey(row, index), approved: null },
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
export const sweepProposals = Effect.fn("Proposals.sweep")(function* () {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select().from(operationProposals)
    .where(and(eq(operationProposals.status, "approved"), isNull(operationProposals.halt), gt(operationProposals.decidedAt, sql`${checkedNow()} - make_interval(secs => ${SWEEP_WINDOW_MS / 1000})`)));
  yield* Effect.forEach(rows, continueProposal, { discard: true });
  return rows.map((row) => row.id);
});

/** `versions` are what the decider's passkey signed for a Deploy that Servers run only signed. */
export type ProposalDecision = { readonly approve: { readonly versions?: readonly PostedVersion[] } } | { readonly reject: { readonly reason?: string } };

type Decider = Actor & { readonly organization: { readonly id: string; readonly slug: string } };

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
  const access = yield* requireAccessById({ userId: decider.userId }, row.organizationId, "changes:deploy");
  const isProtected = yield* environmentProtected(row.organizationId, row.project, row.environmentId).pipe(Effect.catchTag("StoreRefused", () => Effect.succeed(true)));
  if (isProtected && (access.kind !== "member" || !protector(access))) return refuse("Only an owner or admin can approve a deploy to a Protected Environment.");
  const planned = yield* prepareDeployTap(decider, { organizationSlug: decider.organization.slug, environment: command.environment, services: command.services ?? [], classify: true });
  if (planned.kind === "refused") return refuse(isPending(planned.details) ? "A password has no value yet. Approve this deploy on its approval page." : planned.message);
  if (step.deploy.review === undefined) return refuse("Approve this deploy on its approval page.");
  if (command.version !== null && command.version !== undefined && !namesReview(planned.plan.version, command.version)) return { moved: true } as const;
  // The asker's own versions still count: the decider signs only the Services those leave open.
  const signed = new Set(versions.map(versionService));
  const kept = (command.versions ?? []).filter((version) => !signed.has(versionService(version)));
  const covered = new Set([...kept, ...versions].map(versionService));
  const open = (planned.plan.unsigned ?? []).filter(({ service }) => !covered.has(service));
  if (open.length > 0 && planned.tap.kind === "refused") return refuse(planned.tap.message);
  if (open.length > 0 && planned.tap.kind === "ready") return refuse("Sign this deploy with your passkey on its approval page.");
  return { step: { ...step, deploy: { ...step.deploy, command: { ...ruleless(command), versions: [...kept, ...versions] } } } satisfies DeployStep };
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
  const decided = { decidedByUserId: decider.userId, decidedAt: sql`now()`, updatedAt: sql`now()` };
  const deny = (reason: string | null, withdrawn = false) => drizzle.update(operationProposals).set({ status: "denied", reason, withdrawn, ...decided })
    .where(and(eq(operationProposals.id, row.id), eq(operationProposals.status, "presented"))).returning();
  // Each step runs as the asker, so in a Protected Environment only someone who can protect it decides a Deploy, stop
  // or restart: approves it, or rejects another member's request, which ends the ask for good. The one who asked may
  // always withdraw it: their rejection of their own request runs nothing.
  const withdraws = "reject" in decision && row.requestedByUserId !== null && row.requestedByUserId === decider.userId;
  if (!withdraws && !(yield* decides(decider.userId, row))) {
    // A Deploy's approval says it in the Deploy's own words.
    const [first] = row.steps;
    if ("reject" in decision || first === undefined || !isDeploy(first)) return { ok: false, message: OWNER_OR_ADMIN } satisfies Decided;
  }
  if ("reject" in decision) {
    const [denied] = yield* deny(decision.reject.reason?.trim() || null, withdraws);
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
    steps = [checked.step];
  }
  const [approved] = yield* drizzle.update(operationProposals).set({ status: "approved", steps, ...decided })
    .where(and(eq(operationProposals.id, row.id), eq(operationProposals.status, "presented"))).returning();
  if (approved === undefined) return settled((yield* readRow(decider.organization.id, id)) ?? row);
  const start = yield* runStep(approved, 0);
  if (!("halt" in start)) return { ok: true, proposal: proposalView(approved, yield* actsOf(approved)) } satisfies Decided;
  // A Deploy that can't go is denied, not left waiting: its signed versions would outlive the plan they signed.
  if (first !== undefined && isDeploy(first)) return { ok: false, message: `It didn't run: ${start.halt.reason}` } satisfies Decided;
  yield* drizzle.update(operationProposals)
    .set({ status: "presented", halt: null, decidedByUserId: null, decidedAt: null, updatedAt: sql`now()` })
    .where(and(eq(operationProposals.id, row.id), eq(operationProposals.status, "approved")));
  return { ok: false, message: `It didn't run: ${start.halt.reason} It's still waiting for approval.` } satisfies Decided;
});

/** Deny a waiting Deploy for `reason`, unless it moved past `version` or was decided meanwhile. */
const denyWaiting = Effect.fn("Proposals.denyWaiting")(function* (decider: Decider, row: ProposalRow, version: string | null | undefined, reason: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(operationProposals).set({ status: "denied", reason: reason.slice(0, 1024), decidedByUserId: decider.userId, decidedAt: sql`now()`, updatedAt: sql`now()` }).where(and(
    eq(operationProposals.id, row.id), eq(operationProposals.status, "presented"),
    sql`${operationProposals.steps}->0->'deploy'->'command'->>'version' = ${version ?? ""}`,
  ));
  return { ok: false, message: reason } as const;
});

/**
 * Make a Deploy's Pending values. The approval page sealed each one; they land in one edit at exactly the revision the
 * page saw the proposal bound to, so no other edit slips in, and the revision it wrote is recorded on the proposal at
 * once. Then the Store's review is read again as the asker, so a Volume loss the filled plan names is bound and shown
 * before the tap, and the proposal is bound one revision on, so a refresh resumes at the signing. A page whose fill
 * this proposal already recorded resumes it and makes nothing. Any other edit since denies the proposal.
 */
export const fillDeploy = Effect.fn("Proposals.fillDeploy")(function* (decider: Decider, id: string, seen: string | null, changes: readonly Change[]) {
  const row = yield* readRow(decider.organization.id, id);
  const [step] = row?.steps ?? [];
  if (row === undefined || row.status !== "presented" || step === undefined || !isDeploy(step) || row.requestedByUserId === null) return { ok: false, message: "This deploy isn't waiting for approval." } as const;
  const { command, filled } = step.deploy;
  if (filled !== undefined) return yield* rebindFill(decider, row, step, filled);
  const bound = /^(\d+):/u.exec(command.version ?? "")?.[1];
  if (bound === undefined) return yield* denyWaiting(decider, row, command.version, DEPLOY_MOVED);
  if (seen !== command.version) return { ok: false, message: "This deploy changed since this page loaded. Reload it." } as const;
  const written = yield* writeStoreAsMember(decider, decider.organization.slug, { command: "edit", environment: command.environment, expect: Number(bound), changes: [...changes] });
  if (!written.ok && written.refusal.code !== "conflict") return { ok: false, message: written.refusal.message } as const;
  const { drizzle } = yield* Database;
  if (written.ok && written.value.written === "edited") {
    const revision = written.value.environment.revision;
    const [recorded] = yield* drizzle.update(operationProposals).set({ steps: sql`jsonb_set(${operationProposals.steps}, '{0,deploy,filled}', to_jsonb(${revision}::int))`, updatedAt: sql`now()` }).where(and(
      eq(operationProposals.id, row.id), eq(operationProposals.status, "presented"),
      sql`${operationProposals.steps}->0->'deploy'->'command'->>'version' = ${seen}`, sql`${operationProposals.steps}->0->'deploy'->'filled' is null`,
    )).returning();
    const [now] = recorded?.steps ?? [];
    return recorded !== undefined && now !== undefined && isDeploy(now) ? yield* rebindFill(decider, recorded, now, revision) : { ok: false, message: DEPLOY_MOVED } as const;
  }
  // The revision moved first. Only this proposal's own fill, recorded by the page that made it, lets this page go on.
  const after = yield* readRow(decider.organization.id, id);
  const [now] = after?.steps ?? [];
  if (after?.status === "presented" && now !== undefined && isDeploy(now) && now.deploy.filled === Number(bound) + 1) return yield* rebindFill(decider, after, now, now.deploy.filled);
  return yield* denyWaiting(decider, row, command.version, DEPLOY_MOVED);
});

/**
 * Bind a Deploy whose Pending values this proposal's fill wrote at `revision` to the Store's review of the filled plan,
 * read as the asker. Once bound, a page that comes back finds it bound and goes on to the signing.
 */
const rebindFill = Effect.fn("Proposals.rebindFill")(function* (decider: Decider, row: ProposalRow, step: DeployStep, revision: number) {
  const { command } = step.deploy;
  if (/^(\d+):/u.exec(command.version ?? "")?.[1] === String(revision)) return { ok: true, revision } as const;
  if (row.requestedByUserId === null) return yield* denyWaiting(decider, row, command.version, "Whoever asked is no longer in the Organization.");
  const asker = yield* askerNow(row, row.requestedByUserId).pipe(
    Effect.catchIf((error) => error instanceof NotFound || error instanceof Forbidden, (error) => Effect.succeed({ refusal: error.message })),
  );
  if (asker.refusal !== undefined) return yield* denyWaiting(decider, row, command.version, asker.refusal);
  const reviewed = yield* reviewDeploy(row.organizationId, row.requestedByUserId, asker.gate, { ...command, version: moved(command.version, revision) });
  if (reviewed === null || reviewed.refused !== undefined) return yield* denyWaiting(decider, row, command.version, reviewed?.refused?.message ?? DEPLOY_MOVED);
  const next: DeployStep = { ...step, deploy: { ...step.deploy, command: reviewed.command, review: reviewed.review, filled: revision } };
  const { drizzle } = yield* Database;
  const [saved] = yield* drizzle.update(operationProposals).set({ steps: [next], updatedAt: sql`now()` }).where(and(
    eq(operationProposals.id, row.id), eq(operationProposals.status, "presented"),
    sql`${operationProposals.steps}->0->'deploy'->'command'->>'version' = ${command.version ?? ""}`,
  )).returning({ id: operationProposals.id });
  if (saved !== undefined) return { ok: true, revision } as const;
  // Another page bound the same fill first: this one resumes on theirs.
  const [now] = (yield* readRow(row.organizationId, row.id))?.steps ?? [];
  return now !== undefined && isDeploy(now) && now.deploy.command.version === reviewed.command.version ? { ok: true, revision } as const : { ok: false, message: DEPLOY_MOVED } as const;
});
