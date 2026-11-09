import "@tanstack/react-start/server-only";
import type { Operation, ServerRef, ServiceVerb, Signed } from "@ployz/sdk";
import { and, desc, eq, inArray, isNull, lt, or, sql } from "drizzle-orm";
import { Data, Effect } from "effect";
import { createServiceActRequestedEvent, type ServiceActRequestedEventData } from "#/modules/inngest/events";
import type { Admission } from "#/modules/operations/keyed.server";
import { enqueueEvent } from "#/modules/operations/outbox.server";
import { PROPOSAL_CAPABILITY } from "#/modules/proposals/proposal";
import { requirements, ServerLacks, serverSupports } from "#/modules/runtime/daemon-compat";
import { firstRuntimeFrame, OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { type PloyzSdkError, refusedApproval, rpcErrorCode } from "#/modules/runtime/ployz.server";
import {
  type ActiveServiceActState,
  alreadyThere,
  copiesIn,
  endOf,
  enforcesIn,
  APPROVAL_RAN_OUT,
  legOf,
  legsOf,
  type LegStep,
  needsNewerPloyz,
  progressOf,
  ranOut,
  SERVICE_ACT_PENDING_LIMIT_MS,
  SERVICE_ACT_RUNNING_LIMIT_MS,
  type ServiceActBefore,
  type ServiceActLeg,
  type ServiceActState,
  START_APPROVAL_CAPABILITY,
  strandedByStop,
  takes,
} from "#/modules/service-act/service-act";
import { serviceAct } from "#/modules/service-act/tables";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";

const SESSION_GRANT_CAPABILITY = "ployz.restart.session-grant.v2";

type ActRow = typeof serviceAct.$inferSelect;
type ActRequest = ServiceActRequestedEventData;

const ACTIVE_STATES = ["accepted", "running"] as const satisfies readonly ActiveServiceActState[];

export class ServiceActUnreachable extends Data.TaggedError("ServiceActUnreachable")<{ readonly cause: unknown }> {}

const seconds = (at: Date) => Math.floor(at.getTime() / 1000);

export const operationOf = (row: ActRow): Operation => ({
  attempt: row.id,
  run: `${row.verb} ${row.service}`,
  state: row.state,
  requested_at: seconds(row.requestedAt),
  ended_at: row.endedAt === null ? null : seconds(row.endedAt),
  reason: row.reason,
  servers: row.legs.map(legOf),
  kept: [],
  unanswered: row.before?.unanswered.map(({ name }) => name) ?? [],
});

export const requestServiceAct = Effect.fn("ServiceAct.request")(function* (
  caller: { readonly organizationId: string; readonly userId: string },
  input: {
    readonly attemptId: string;
    readonly environmentId: string;
    readonly service: string;
    readonly namespace: string;
    readonly runtime: string;
    readonly verb: ServiceVerb;
    readonly only: ServerRef | null;
    readonly digest: string;
    readonly approvals?: Readonly<Record<string, Signed>>;
    readonly proposal?: { readonly id: string; readonly step: number };
  },
) {
  const { drizzle } = yield* Database;
  const [inserted] = yield* drizzle.insert(serviceAct).values({
    id: input.attemptId,
    organizationId: caller.organizationId,
    environmentId: input.environmentId,
    service: input.service,
    namespace: input.namespace,
    runtime: input.runtime,
    verb: input.verb,
    only: input.only,
    digest: input.digest,
    approvals: input.approvals ?? null,
    proposalId: input.proposal?.id ?? null,
    proposalStep: input.proposal?.step ?? null,
    requestedByUserId: caller.userId,
  }).onConflictDoNothing().returning();
  if (inserted !== undefined) {
    yield* enqueueEvent(createServiceActRequestedEvent({ actId: inserted.id, organizationId: inserted.organizationId }));
    return { ok: true, id: inserted.id } satisfies Admission;
  }
  const [replayed] = yield* drizzle.select().from(serviceAct)
    .where(and(eq(serviceAct.organizationId, caller.organizationId), eq(serviceAct.id, input.attemptId)));
  if (replayed !== undefined) return { ok: true, id: replayed.id } satisfies Admission;
  const run = yield* activeServiceAct(caller.organizationId, input);
  return {
    ok: false,
    refusal: run === null
      ? { code: "conflict", message: `${input.service} just finished another run. Try again.`, details: { run } }
      : busyRefusal(input.service, run),
  } satisfies Admission;
});

/** The run that holds `runtime` in its Environment now, if one does. */
export const activeServiceAct = Effect.fn("ServiceAct.active")(function* (
  organizationId: string,
  input: { readonly environmentId: string; readonly runtime: string },
) {
  const { drizzle } = yield* Database;
  const [active] = yield* drizzle.select().from(serviceAct).where(and(
    eq(serviceAct.organizationId, organizationId),
    eq(serviceAct.environmentId, input.environmentId),
    eq(serviceAct.runtime, input.runtime),
    inArray(serviceAct.state, ACTIVE_STATES),
  )).limit(1);
  return active === undefined ? null : operationOf(active);
});

export const busyRefusal = (service: string, run: Operation) =>
  ({ code: "conflict", message: `${service} is busy: ${run.run} is still running.`, details: { run } }) as const;

export const serviceActExists = Effect.fn("ServiceAct.exists")(function* (organizationId: string, actId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ id: serviceAct.id }).from(serviceAct)
    .where(and(eq(serviceAct.organizationId, organizationId), eq(serviceAct.id, actId)));
  return row !== undefined;
});

const readAct = Effect.fn("ServiceAct.read")(function* (actId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select().from(serviceAct).where(eq(serviceAct.id, actId));
  if (row === undefined) return yield* new NotFound({ message: "The service run was not found." });
  return row;
});

const owned = (request: ActRequest, runId: string) =>
  and(eq(serviceAct.id, request.actId), eq(serviceAct.organizationId, request.organizationId), eq(serviceAct.inngestRunId, runId));

export type ClaimedAct = { kind: "claimed"; verb: ServiceVerb; legs: ServiceActLeg[] } | { kind: "settled"; state: ServiceActState };

export const claimServiceAct = Effect.fn("ServiceAct.claim")(function* (request: ActRequest, runId: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(serviceAct).set({ inngestRunId: runId }).where(and(
    eq(serviceAct.id, request.actId),
    eq(serviceAct.organizationId, request.organizationId),
    eq(serviceAct.state, "accepted"),
    or(isNull(serviceAct.inngestRunId), eq(serviceAct.inngestRunId, runId)),
  ));
  const row = yield* readAct(request.actId);
  if (row.organizationId !== request.organizationId) return yield* new NotFound({ message: "The service run was not found." });
  if (row.inngestRunId !== runId || (row.state !== "accepted" && row.state !== "running")) {
    return { kind: "settled", state: row.state } satisfies ClaimedAct;
  }
  if (row.state === "running") return { kind: "claimed", verb: row.verb, legs: row.legs } satisfies ClaimedAct;
  const session = yield* (yield* OrganizationRuntime).open(request.organizationId);
  if (session.status !== "connected") return yield* new ServiceActUnreachable({ cause: session.status });
  const before: ServiceActBefore = yield* session.connected.observeService(row.namespace, row.runtime);
  const legs = legsOf(before, row.only, row.verb);
  const [claimed] = yield* drizzle.update(serviceAct).set({ state: "running", before, legs, startedAt: new Date() })
    .where(and(owned(request, runId), eq(serviceAct.state, "accepted"))).returning();
  if (claimed === undefined) return { kind: "settled", state: (yield* readAct(request.actId)).state } satisfies ClaimedAct;
  return { kind: "claimed", verb: claimed.verb, legs: claimed.legs } satisfies ClaimedAct;
}, Effect.scoped);

/** What a start leg records for each answer the Server gives. */
const STARTED = {
  serving: (leg: ServiceActLeg) => ["done", leg.reason] as const,
  unconfirmed: (leg: ServiceActLeg) => ["unknown", `${leg.server} started it, but it isn't serving.`] as const,
  unhealthy: (leg: ServiceActLeg) => ["failed", `${leg.server} started it, but it isn't healthy.`] as const,
} satisfies Record<"serving" | "unconfirmed" | "unhealthy", (leg: ServiceActLeg) => readonly [LegStep, string | null]>;

type LegSupport = { readonly ensure: boolean | "unknown"; readonly sessionGrant: boolean; readonly startApproval: boolean; readonly proposal: boolean };

/**
 * Whether each Server's record says it runs EnsureContainer, takes a browser's approval, takes one for a start, and
 * takes a proposal's step. A Server Cloud can't read is attempted, without an approval: one from before session grants
 * would refuse the whole request for carrying it.
 */
const legSupport = (organizationId: string) => firstRuntimeFrame(organizationId).pipe(
  Effect.map((frame) => new Map<string, LegSupport>((frame?.machines ?? []).map(({ machine }) => {
    const capabilities = machine.runtime.capabilities ?? [];
    return [machine.id, {
      ensure: serverSupports({ capabilities }, requirements.ensureContainer),
      sessionGrant: capabilities.includes(SESSION_GRANT_CAPABILITY),
      startApproval: capabilities.includes(START_APPROVAL_CAPABILITY),
      proposal: capabilities.includes(PROPOSAL_CAPABILITY),
    }];
  }))),
  Effect.orElseSucceed(() => new Map<string, LegSupport>()),
);

const writeLegs = (request: ActRequest, runId: string, legs: ServiceActLeg[]) =>
  Effect.flatMap(Database, ({ drizzle }) =>
    drizzle.update(serviceAct).set({ legs }).where(and(owned(request, runId), eq(serviceAct.state, "running"))).returning({ id: serviceAct.id }))
    .pipe(Effect.map((rows) => rows.length > 0));

/**
 * Whether a Server running a copy of the Service reports that it enforces, so a restart sent without approvals would
 * only be refused there. Cloud can't tell when it can't read the Servers, and sends it anyway.
 */
export const serviceEnforces = Effect.fn("ServiceAct.enforces")(function* (organizationId: string, service: { readonly namespace: string; readonly runtime: string }) {
  const frame = yield* firstRuntimeFrame(organizationId).pipe(Effect.orElseSucceed(() => null));
  return frame !== null && enforcesIn(frame, `${service.namespace}/${service.runtime}`);
});

/**
 * What a browser needs to approve a restart or stop of the Service: its copies, the Servers, and which of them run the
 * Environment. Null when Cloud can't read the Servers.
 */
export const actTarget = Effect.fn("ServiceAct.actTarget")(function* (
  organizationId: string,
  service: { readonly namespace: string; readonly runtime: string },
) {
  const frame = yield* firstRuntimeFrame(organizationId).pipe(Effect.orElseSucceed(() => null));
  if (frame === null) return null;
  return {
    runtime: service.runtime,
    copies: copiesIn(frame, `${service.namespace}/${service.runtime}`).map((copy) => ({ id: copy.container_id, machineId: copy.machine_id, hook: copy.hook })),
    machines: frame.machines.map(({ machine }) => ({
      id: machine.id,
      name: machine.name,
      capabilities: machine.runtime.capabilities ?? [],
      sealing: machine.runtime.sealing ?? null,
      deviceEnforcement: machine.runtime.device_enforcement ?? null,
    })),
    environmentMachines: [...new Set(frame.containers.filter((container) => container.namespace === service.namespace).map((container) => container.machine_id))],
  };
});

/**
 * Why a stop of the Service is refused before anything stops: a Server running it enforces but can't take a start
 * approval, so the Service could never be started again from Ployz Cloud. Null when the stop may go. A stop that
 * carries approvals is for Servers that enforce, so when Cloud can't read the Servers it refuses rather than guess;
 * one without approvals goes, and each Server refuses what it can't take.
 */
export const strandingStop = Effect.fn("ServiceAct.strandingStop")(function* (
  organizationId: string,
  service: { readonly namespace: string; readonly runtime: string },
  approved: boolean,
) {
  const target = yield* actTarget(organizationId, service);
  if (target === null) return approved ? "Cloud couldn't check the servers. Try again." : null;
  const server = strandedByStop(target.copies, target.machines);
  return server === null ? null : needsNewerPloyz(server, "stop");
});

export const actOnContainer = Effect.fn("ServiceAct.leg")(function* (
  request: ActRequest,
  runId: string,
  container: string,
  side: "stop" | "start",
) {
  const row = yield* readAct(request.actId);
  if (row.state !== "running" || row.inngestRunId !== runId) return row.legs;
  const states = new Map((row.before?.copies ?? []).map((copy) => [copy.container, copy.state]));
  const legs = row.legs.map((leg) => ({ ...leg }));
  const write = (leg: ServiceActLeg, step: LegStep, reason: string | null = leg.reason) =>
    Effect.suspend(() => {
      leg[side] = step;
      leg.reason = reason;
      return writeLegs(request, runId, legs);
    });
  const session = yield* (yield* OrganizationRuntime).open(request.organizationId);
  const support = yield* legSupport(request.organizationId);
  for (const leg of legs) {
    // A `sent` leg is a step that crashed around its call: the want is safe to send again.
    if (leg.container !== container || !takes(leg, side) || leg[side] !== null && leg[side] !== "sent") continue;
    if (alreadyThere(row.verb, side, states.get(leg.container))) {
      if (!(yield* write(leg, "already_there"))) return legs;
      continue;
    }
    // Checked again at each leg: a restart's start leg can fall past the window its stop leg was inside.
    if (ranOut(row.approvals?.[leg.container], Date.now())) {
      if (!(yield* write(leg, "failed", APPROVAL_RAN_OUT))) return legs;
      continue;
    }
    if (session.status !== "connected") {
      if (!(yield* write(leg, "failed", `Cloud couldn't reach ${leg.server}.`))) return legs;
      continue;
    }
    const supported = support.get(leg.machine);
    if (supported?.ensure === false) {
      if (!(yield* write(leg, "failed", needsNewerPloyz(leg.server, side)))) return legs;
      continue;
    }
    if (!(yield* write(leg, "sent"))) return legs;
    // A start approval, or a proposal's step, reads as malformed to a Server from before them, so it never goes there.
    const made = row.approvals?.[leg.container];
    const takesApproval = supported?.sessionGrant === true && (row.verb !== "start" || supported.startApproval)
      && (made?.proposal === undefined || supported.proposal);
    const approval = takesApproval ? made ?? null : null;
    const withheld = approval === null && row.approvals?.[leg.container] !== undefined;
    // With the Servers' records unread, Cloud can't tell whether this one takes an approval, so it sent none.
    const unread = withheld && supported === undefined;
    const sent: Effect.Effect<readonly [LegStep, string | null], PloyzSdkError> = side === "stop"
      ? session.connected.stopContainer(leg.machine, leg.container, approval).pipe(Effect.as(["done", leg.reason] as const))
      : session.connected.startContainer(leg.machine, leg.container, approval).pipe(
        Effect.map((started) => STARTED[started](leg)),
      );
    const [step, reason] = yield* sent.pipe(
      Effect.catch((error) => Effect.succeed(
        error instanceof ServerLacks
          ? ["failed", needsNewerPloyz(leg.server, side)] as const
          : refusedApproval(error)
          ? ["failed", unread ? `Cloud couldn't check ${leg.server}. Try again.`
            : withheld ? needsNewerPloyz(leg.server, side)
            : approval === null ? `${leg.server} needs your passkey to ${row.verb} it.`
            : `${leg.server} didn't accept the approval.`] as const
          : rpcErrorCode(error) === "unavailable"
          ? ["unknown", `${leg.server} didn't answer.`] as const
          : ["failed", `${leg.server} couldn't ${side} it.`] as const,
      )),
    );
    if (!(yield* write(leg, step, reason))) return legs;
  }
  return legs;
}, Effect.scoped);

export const recordServiceAct = Effect.fn("ServiceAct.record")(function* (request: ActRequest, runId: string) {
  const { drizzle } = yield* Database;
  const row = yield* readAct(request.actId);
  if (row.state !== "running" || row.inngestRunId !== runId || row.before === null) return { actId: row.id, state: row.state };
  const end = endOf(row.verb, row.before, row.legs, row.only);
  const [written] = yield* drizzle.update(serviceAct).set({ ...end, endedAt: new Date() })
    .where(and(owned(request, runId), eq(serviceAct.state, "running"))).returning({ state: serviceAct.state });
  return { actId: row.id, state: written?.state ?? (yield* readAct(request.actId)).state };
});

const RUN_ENDS = {
  failure: { accepted: "The run never started.", running: "Cloud lost track of the run." },
  cancellation: { accepted: "The run was cancelled before it started.", running: "The run was cancelled while it ran." },
} as const;

export const closeServiceActRun = Effect.fn("ServiceAct.closeRun")(function* (
  runId: string,
  reason: keyof typeof RUN_ENDS,
  actId?: string,
) {
  const { drizzle } = yield* Database;
  const accepted = sql`${serviceAct.state} = 'accepted'`;
  const unbound = actId === undefined ? undefined : and(
    eq(serviceAct.id, actId),
    eq(serviceAct.state, "accepted"),
    or(isNull(serviceAct.inngestRunId), eq(serviceAct.inngestRunId, runId)),
  );
  const rows = yield* drizzle.update(serviceAct).set({
    state: sql<ServiceActState>`case when ${accepted} then 'failed' else 'unknown' end`,
    reason: sql<string>`case when ${accepted} then ${RUN_ENDS[reason].accepted} else ${RUN_ENDS[reason].running} end`,
    endedAt: new Date(),
  })
    .where(or(and(eq(serviceAct.inngestRunId, runId), inArray(serviceAct.state, ACTIVE_STATES)), unbound))
    .returning({ id: serviceAct.id });
  return rows.length;
});

export const closeStaleServiceActs = Effect.fn("ServiceAct.closeStale")(function* () {
  const { drizzle } = yield* Database;
  const now = Date.now();
  const unknown = yield* drizzle.update(serviceAct).set({ state: "unknown", reason: RUN_ENDS.failure.running, endedAt: new Date() })
    .where(and(eq(serviceAct.state, "running"), lt(serviceAct.startedAt, new Date(now - SERVICE_ACT_RUNNING_LIMIT_MS))))
    .returning({ id: serviceAct.id });
  const failed = yield* drizzle.update(serviceAct).set({ state: "failed", reason: RUN_ENDS.failure.accepted, endedAt: new Date() })
    .where(and(eq(serviceAct.state, "accepted"), lt(serviceAct.requestedAt, new Date(now - SERVICE_ACT_PENDING_LIMIT_MS))))
    .returning({ id: serviceAct.id });
  return { failed: failed.length, unknown: unknown.length };
});

const actsOf = (
  organizationId: string,
  subject: { readonly environment: string; readonly service: string },
  attempt: string | null,
  limit: number,
) =>
  Effect.flatMap(Database, ({ drizzle }) =>
    drizzle.select().from(serviceAct).where(and(
      eq(serviceAct.organizationId, organizationId),
      eq(serviceAct.environmentId, subject.environment),
      eq(serviceAct.service, subject.service),
      attempt === null ? undefined : eq(serviceAct.id, attempt),
    )).orderBy(desc(serviceAct.requestedAt), desc(serviceAct.id)).limit(limit));

export const serviceOperations = Effect.fn("ServiceAct.operations")(function* (
  organizationId: string,
  subject: { readonly environment: string; readonly service: string },
  attempt: string | null,
  limit: number,
) {
  return (yield* actsOf(organizationId, subject, attempt, limit)).map(operationOf);
});

export const latestServiceAct = Effect.fn("ServiceAct.latest")(function* (
  organizationId: string,
  subject: { readonly environment: string; readonly service: string },
) {
  const [row] = yield* actsOf(organizationId, subject, null, 1);
  return row === undefined ? null : progressOf(row);
});
