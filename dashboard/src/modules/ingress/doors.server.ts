import "@tanstack/react-start/server-only";
import { randomUUID } from "node:crypto";
import { and, desc, eq, inArray, isNull, lt, or, sql } from "drizzle-orm";
import type { JsonValue } from "@ployz/sdk";
import { Effect, Schedule, Schema } from "effect";
import { operationDigest, type OperationAsked } from "#/modules/approvals/approvals.server";
import { loadClusterDomain } from "#/modules/cluster-domain/cluster-domain.server";
import { doorsOpenOn } from "#/modules/cluster-domain/ingress-running";
import { loadCloudflareSetup } from "#/modules/cloudflare/source.server";
import { readStore } from "#/modules/config-store/config-store.server";
import { deployToFirstServer } from "#/modules/config-store/store-deployment.server";
import { publishedOf } from "#/modules/config-store/domain-evidence.server";
import { owners } from "#/modules/config-store/store-lookup.server";
import { StoreRefused } from "#/modules/config-store/store.contract";
import {
  type CloudflareSetup,
  type ClusterIngressObservation,
  type ClusterIngressView,
  DOORS_PENDING_LIMIT_MS,
  DOORS_RUNNING_LIMIT_MS,
  type DoorsState,
  type DoorsWant,
  type LatestDoorsChange,
  type MovedHostname,
  movedHostnames,
  observationGap,
  redeployScope,
  switchProblem,
  wantWords,
} from "#/modules/ingress/doors";
import { activeDoorsAttempt, lockIngressRows } from "#/modules/ingress/doors.lock.server";
import { type RedeployPorts, redeployMoved, runSwitch } from "#/modules/ingress/doors.run";
import { ingressDoorsAttempt, organizationCloudflareIngress } from "#/modules/ingress/tables";
import { createIngressDoorsRequestedEvent, type IngressDoorsRequestedEventData } from "#/modules/inngest/events";
import { offlineServers } from "#/modules/machines/server-status";
import { refusalWords } from "#/modules/machines/server-web-traffic.server";
import { enqueueEvent } from "#/modules/operations/outbox.server";
import { firstRuntimeFrame, OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { ServerLacks } from "#/modules/runtime/daemon-compat";
import { PloyzProviderError, type PloyzSession } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";
import { Conflict, NotFound } from "#/server/public-error";

type AttemptRow = typeof ingressDoorsAttempt.$inferSelect;
type DoorsRequest = IngressDoorsRequestedEventData;
export type DoorsRunReply = { readonly attemptId: string; readonly state: DoorsState };

const ACTIVE_STATES: readonly DoorsState[] = ["pending", "running"];
const MESSAGE_LIMIT = 1024;

const within = (text: string) => (text.length > MESSAGE_LIMIT ? `${text.slice(0, MESSAGE_LIMIT - 1)}…` : text);
const failed = (failure: string) => ({ state: "failed", failure: within(failure), endedAt: new Date() }) as const;
const finished = () => ({ state: "finished", failure: null, endedAt: new Date() }) as const;

/** A row as the page reads it. The check constraint keeps each state's evidence; a row without it is a defect. */
export function latestChangeOf(row: AttemptRow): Effect.Effect<LatestDoorsChange> {
  const { id: attemptId, want } = row;
  const missing = (what: string) => Effect.die(new Error(`Door switch ${attemptId} is ${row.state} without its ${what}.`));
  switch (row.state) {
    case "pending":
      return Effect.succeed({ attemptId, want, state: "pending", requestedAt: row.requestedAt.toISOString() });
    case "running":
      return row.startedAt === null
        ? missing("start time")
        : Effect.succeed({ attemptId, want, state: "running", startedAt: row.startedAt.toISOString(), stage: row.stage });
    case "finished":
      return row.endedAt === null ? missing("end time") : Effect.succeed({ attemptId, want, state: "finished", endedAt: row.endedAt.toISOString() });
    case "failed":
      return row.endedAt === null || row.failure === null
        ? missing("failure")
        : Effect.succeed({ attemptId, want, state: "failed", endedAt: row.endedAt.toISOString(), failure: row.failure });
  }
}

/** The Organization's latest switch, whatever its state, as the page reads it. Null before the first. */
const latestChange = Effect.fn("IngressDoors.latest")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const [latest] = yield* drizzle.select().from(ingressDoorsAttempt).where(eq(ingressDoorsAttempt.organizationId, organizationId))
    .orderBy(desc(ingressDoorsAttempt.requestedAt), desc(ingressDoorsAttempt.id)).limit(1);
  return latest === undefined ? null : yield* latestChangeOf(latest);
});

/** The Cluster's ingress as the database holds it: its Cloudflare setup, the latest switch and the Hosted name. */
export const readClusterIngress = Effect.fn("IngressDoors.read")(function* (organizationId: string) {
  const setup = yield* loadCloudflareSetup(organizationId);
  const change = yield* latestChange(organizationId);
  const hosted = yield* loadClusterDomain(organizationId);
  return { setup, change, hostedName: hosted?.name ?? null } satisfies ClusterIngressView;
});

/**
 * What one look at the Servers finds: who takes web traffic, which Doors each holds open, and the hostnames published.
 * Null when none answers. Partial when a row wasn't read, or when a Server reads Offline: its rows are as it last
 * reported them, so what it serves now is unknown.
 */
export const observeClusterIngress = Effect.fn("IngressDoors.observe")(function* (organizationId: string) {
  const frame = yield* firstRuntimeFrame(organizationId).pipe(Effect.catch(() => Effect.succeed(null)));
  if (frame === null) return null;
  const open = doorsOpenOn(frame);
  const offline = offlineServers(frame.machines.map(({ machine, membership }) =>
    ({ id: machine.id, membership, runningBuilds: machine.runtime.running_builds, unanswered: frame.unanswered.includes(machine.id) })));
  return {
    servers: frame.machines.filter(({ machine }) => machine.accepts_ingress)
      .map(({ machine }) => ({ id: machine.id, name: machine.name, doors: [...(open.get(machine.id) ?? [])].sort() })),
    hostnames: publishedOf(frame),
    partial: frame.incomplete_ids.machines.length > 0 || frame.incomplete_ids.containers.length > 0 || offline.size > 0,
  } satisfies ClusterIngressObservation;
});

/**
 * What a switch to `to` moves, from what the Servers showed: every generated hostname they publish under the source the
 * switch leaves, with the Service publishing it. Nothing before Cloudflare is set up or the Hosted name exists.
 */
const movesOf = (setup: CloudflareSetup | null, hosted: string | null, seen: ClusterIngressObservation, to: DoorsWant) => {
  const zone = setup?.zone ?? null;
  if (zone === null || hosted === null) return [];
  return to === "tunnel" ? movedHostnames(seen.hostnames, hosted, zone) : movedHostnames(seen.hostnames, zone, hosted);
};

/** One look at the Cluster for a switch to `to`: its setup, Hosted name, what the Servers showed and the moves that follow. */
const switchSeen = Effect.fn("IngressDoors.seen")(function* (organizationId: string, to: DoorsWant) {
  const setup = yield* loadCloudflareSetup(organizationId);
  const hosted = (yield* loadClusterDomain(organizationId))?.name ?? null;
  const seen = yield* observeClusterIngress(organizationId);
  return { setup, hosted, seen, moving: seen === null ? [] : movesOf(setup, hosted, seen, to) };
});

/**
 * What switching the Cluster to `to` asks a human: every generated hostname moves between the Hosted domain and the
 * zone, so each one an Environment publishes now is a domain it stops serving. Asked of a CLI or an agent whenever
 * Cloud can name the move, and always, since a switch changes where every address points.
 */
export const switchAsked = Effect.fn("IngressDoors.asked")(function* (organizationId: string, to: DoorsWant) {
  const { setup, hosted, moving } = yield* switchSeen(organizationId, to);
  return {
    subject: `cluster:${organizationId}`,
    verb: "set_ingress_doors",
    name: to === "tunnel" ? "to Cloudflare" : "to public ports",
    preview: { to, zone: setup?.zone ?? null, hostedName: hosted, hostnames: moving.map(({ from, to: next }) => ({ from, to: next })) },
    effects: moving.map(({ from, namespace, service }) => ({ kind: "removes_domain", name: from, node: from, path: `${namespace}/${service}.managedHostnames` })),
    alwaysAsks: true,
  } satisfies OperationAsked;
});

/** The review before a human confirms: the moves the Servers showed, and why the list may be short when they didn't all answer. */
export const reviewSwitch = Effect.fn("IngressDoors.review")(function* (organizationId: string, to: DoorsWant) {
  const { setup, hosted, seen, moving } = yield* switchSeen(organizationId, to);
  return { moves: moving.map(({ from, to: next }) => ({ from, to: next })), zone: setup?.zone ?? null, hostedName: hosted, gap: observationGap(seen) };
});

const Switched = Schema.Struct({ to: Schema.Literals(["tunnel", "public"]) });

/**
 * A pending switch's digest as the Cluster stands now: none once the Cluster is where it asked to go. A switch refused
 * for now, while Cloudflare is being set up or until it is, keeps its digest, so the approval waits and the request it
 * lets through is refused with the reason, instead of being superseded and asked again at once.
 */
export const freshSwitchDigest = Effect.fn("IngressDoors.freshDigest")(function* (organizationId: string, stored: string, preview: JsonValue) {
  const switched = Schema.decodeUnknownOption(Switched)(preview);
  if (switched._tag === "None") return stored;
  const { to } = switched.value;
  const setup = yield* loadCloudflareSetup(organizationId);
  if (switchProblem(setup, to, yield* latestChange(organizationId)) === null) return operationDigest(yield* switchAsked(organizationId, to));
  const there = to === "tunnel" ? setup?.state === "active" : setup === null || setup.state === "left";
  return there ? null : stored;
});

const enqueueSwitch = (row: AttemptRow) => enqueueEvent(createIngressDoorsRequestedEvent({ attemptId: row.id, organizationId: row.organizationId }));

/**
 * Ask for a switch of the Cluster's web traffic. Its row is written with its run's event, under the lock Cloudflare
 * setup's write takes, so the switch reads the setup it will move. A request while a switch the same way is active
 * joins it (`joined`); one the other way is refused, since the running switch is still moving every address.
 */
export const requestDoorsSwitch = Effect.fn("IngressDoors.request")(function* (
  input: { readonly organizationId: string; readonly userId: string | null; readonly to: DoorsWant },
) {
  const { organizationId } = input;
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    yield* lockIngressRows(organizationId);
    const active = yield* activeDoorsAttempt(organizationId);
    if (active !== null && active.want !== input.to) {
      return yield* new Conflict({ userFacing: true, message: `A switch to ${wantWords(active.want)} is under way. Wait for it to finish, then switch again.` });
    }
    if (active !== null) return { change: yield* latestChangeOf(active), joined: true };
    const setup = yield* loadCloudflareSetup(organizationId);
    const problem = switchProblem(setup, input.to, yield* latestChange(organizationId));
    if (problem !== null) return yield* new Conflict({ userFacing: true, message: problem });
    const { drizzle } = yield* Database;
    const [inserted] = yield* drizzle.insert(ingressDoorsAttempt).values({
      id: randomUUID(), organizationId, requestedByUserId: input.userId, want: input.to,
    }).returning();
    if (inserted === undefined) return yield* Effect.die(new Error("The switch row was not written."));
    yield* enqueueSwitch(inserted);
    return { change: yield* latestChangeOf(inserted), joined: false };
  }));
});

/** Bind the run to its pending row, so only it may claim and end the row. A row no longer pending is settled: the run does nothing. */
export const bindDoorsRun = Effect.fn("IngressDoors.bind")(function* (request: DoorsRequest, runId: string) {
  const { drizzle } = yield* Database;
  const [bound] = yield* drizzle.update(ingressDoorsAttempt).set({ inngestRunId: runId }).where(and(
    eq(ingressDoorsAttempt.id, request.attemptId),
    eq(ingressDoorsAttempt.organizationId, request.organizationId),
    eq(ingressDoorsAttempt.state, "pending"),
    or(isNull(ingressDoorsAttempt.inngestRunId), eq(ingressDoorsAttempt.inngestRunId, runId)),
  )).returning({ id: ingressDoorsAttempt.id });
  if (bound !== undefined) return { kind: "bound" } as const;
  const [row] = yield* drizzle.select({ state: ingressDoorsAttempt.state, organizationId: ingressDoorsAttempt.organizationId })
    .from(ingressDoorsAttempt).where(eq(ingressDoorsAttempt.id, request.attemptId));
  if (row === undefined || row.organizationId !== request.organizationId) return yield* new NotFound({ message: "The switch was not found." });
  return { kind: "settled", state: row.state } as const;
});

const stateOf = Effect.fn("IngressDoors.stateOf")(function* (attemptId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ state: ingressDoorsAttempt.state }).from(ingressDoorsAttempt).where(eq(ingressDoorsAttempt.id, attemptId));
  if (row === undefined) return yield* new NotFound({ message: "The switch was not found." });
  return { attemptId, state: row.state } satisfies DoorsRunReply;
});

/** What a switch that stopped says: the Engine's own refusal when a Server gave one, the Store's when its redeploy was refused. */
const failureWords = (error: { readonly _tag: string }) => {
  if (error instanceof PloyzProviderError || error instanceof ServerLacks) return refusalWords(error);
  if (error instanceof StoreRefused) return error.message;
  return "Your servers didn't answer.";
};

const openSession = Effect.fn("IngressDoors.openSession")(function* (organizationId: string) {
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  return session.status === "connected" ? session.connected : null;
});

/** The Deployment's status once it is no longer in flight. The switch's own time limit bounds the wait. */
const deploymentEnd = (organizationId: string, id: string) =>
  readStore(organizationId, { query: "deployment", id }).pipe(
    Effect.repeat({ schedule: Schedule.spaced("2 seconds"), while: (view) => view.in_flight }),
    Effect.map((view) => view.status),
  );

/**
 * The generated names a switch to `want` moves, read while both Doors are open, or why Cloud can't tell: a Server
 * that didn't answer may publish a name the switch would otherwise leave behind, so the switch stops before its source moves.
 */
const movingNow = Effect.fn("IngressDoors.moving")(function* (organizationId: string, want: DoorsWant) {
  const { seen, moving } = yield* switchSeen(organizationId, want);
  const gap = observationGap(seen);
  return gap === null ? { moves: moving } : { failure: gap };
});

/**
 * Redeploy the Services whose generated names the switch moves, and only those, so each expands under the Cluster
 * Domain's new source, and wait until the Servers serve every one there. Why they don't all, or null once they do:
 * the Door the switch leaves stays open until then.
 */
const redeployMoving = Effect.fn("IngressDoors.redeploy")(function* (organizationId: string, moving: readonly MovedHostname[]) {
  if (moving.length === 0) return null;
  const owned = yield* owners(organizationId);
  const named = yield* Effect.forEach([...new Set(moving.map(({ namespace }) => namespace))], (namespace) => {
    const owner = owned.find((candidate) => candidate.namespace === namespace);
    if (owner === undefined) return Effect.succeed(null);
    return readStore(organizationId, { query: "namespace", environment: { project: owner.project, environment: owner.environment } })
      .pipe(Effect.map((view) => [namespace, view.services] as const));
  });
  const { environments, left } = redeployScope(moving, owned, new Map(named.filter((entry) => entry !== null)));
  const [orphan] = left;
  if (orphan !== undefined) return `${orphan.from} is served by ${orphan.namespace}/${orphan.service}, which isn't a Service Cloud can redeploy. Remove it, then switch again.`;
  const ports: RedeployPorts<RedeployError, RedeployServices> = {
    deploy: ({ project, environment, services }) => Effect.gen(function* () {
      const id = randomUUID();
      const admission = yield* deployToFirstServer(organizationId, { project, environment }, id, services);
      return admission.refused === undefined ? { id } : { refused: admission.message };
    }),
    ended: (id) => deploymentEnd(organizationId, id),
    observe: () => observeClusterIngress(organizationId),
  };
  return yield* redeployMoved(moving, environments, ports);
});

type Admission = ReturnType<typeof deployToFirstServer>;
type Observation = ReturnType<typeof observeClusterIngress>;
type Ending = ReturnType<typeof deploymentEnd>;
type RedeployError = Effect.Error<Admission> | Effect.Error<Ending> | Effect.Error<Observation>;
type RedeployServices = Effect.Services<Admission> | Effect.Services<Ending> | Effect.Services<Observation>;

const applySwitch = Effect.fn("IngressDoors.apply")(function* (session: PloyzSession, request: DoorsRequest, runId: string, want: DoorsWant) {
  const { drizzle } = yield* Database;
  const owned = and(eq(ingressDoorsAttempt.id, request.attemptId), eq(ingressDoorsAttempt.inngestRunId, runId));
  return yield* runSwitch(want, {
    open: (doors) => session.setIngressDoors(doors),
    at: (stage) => drizzle.update(ingressDoorsAttempt).set({ stage }).where(owned).pipe(Effect.asVoid),
    flip: () => drizzle.update(organizationCloudflareIngress).set({ state: want === "tunnel" ? "active" : "left", updatedAt: new Date() })
      .where(eq(organizationCloudflareIngress.organizationId, request.organizationId)).pipe(Effect.asVoid),
    moving: () => movingNow(request.organizationId, want),
    redeploy: (moves) => redeployMoving(request.organizationId, moves),
  });
});

/**
 * Switch once. The row is claimed (`running`) right before the Engine is asked and ended with its answer right after, in
 * one step: a retry that finds it running knows the Engine may have been asked, closes it failed and never asks again.
 * A switch left half done is finished by asking again, since every Door set is idempotent.
 */
export const executeDoorsOnce = Effect.fn("IngressDoors.execute")(function* (request: DoorsRequest, runId: string) {
  const { drizzle } = yield* Database;
  const owned = and(eq(ingressDoorsAttempt.id, request.attemptId), eq(ingressDoorsAttempt.inngestRunId, runId));
  const current = yield* stateOf(request.attemptId);
  if (current.state === "running") {
    yield* drizzle.update(ingressDoorsAttempt).set(failed("Cloud lost track of the switch. Check the section and switch again."))
      .where(and(owned, eq(ingressDoorsAttempt.state, "running")));
    return yield* stateOf(request.attemptId);
  }
  if (current.state !== "pending") return current;
  const [row] = yield* drizzle.select({ want: ingressDoorsAttempt.want }).from(ingressDoorsAttempt).where(eq(ingressDoorsAttempt.id, request.attemptId));
  if (row === undefined) return yield* new NotFound({ message: "The switch was not found." });
  const session = yield* openSession(request.organizationId);
  const [claimed] = yield* drizzle.update(ingressDoorsAttempt).set({ state: "running", startedAt: new Date() })
    .where(and(owned, eq(ingressDoorsAttempt.state, "pending"))).returning({ id: ingressDoorsAttempt.id });
  if (claimed === undefined) return yield* stateOf(request.attemptId);
  const end = session === null
    ? failed("Your servers aren't answering. Try again once they are.")
    : yield* applySwitch(session, request, runId, row.want).pipe(
      Effect.map((ended) => ended.ok ? finished() : failed(ended.failure)),
      Effect.catch((error) => Effect.succeed(failed(failureWords(error)))),
      Effect.catchDefect((defect) => Effect.as(Effect.logError("Door switch died", defect), failed("Cloud lost track of the switch. Check the section and switch again."))),
      Effect.timeoutOrElse({ duration: DOORS_RUNNING_LIMIT_MS, orElse: () => Effect.succeed(failed("Your servers didn't answer in time. Check the section and switch again.")) }),
    );
  const [written] = yield* drizzle.update(ingressDoorsAttempt).set(end)
    .where(and(owned, eq(ingressDoorsAttempt.state, "running"))).returning({ state: ingressDoorsAttempt.state })
    .pipe(Effect.retry({ times: 4, schedule: Schedule.exponential("500 millis") }));
  return written === undefined ? yield* stateOf(request.attemptId) : { attemptId: request.attemptId, state: written.state };
}, Effect.scoped);

const RUN_ENDS = {
  failure: { pending: "The switch never started. Try again.", running: "Cloud lost track of the switch. Check the section and switch again." },
  cancellation: { pending: "The switch was cancelled before it started.", running: "The switch was cancelled while it ran. Check the section and switch again." },
} as const satisfies Record<string, Record<"pending" | "running", string>>;

/** A run that failed for good or was cancelled must not leave its row active. How many rows this call ended. */
export const closeDoorsRun = Effect.fn("IngressDoors.closeRun")(function* (runId: string, reason: keyof typeof RUN_ENDS, attemptId?: string) {
  const { drizzle } = yield* Database;
  const { pending, running } = RUN_ENDS[reason];
  const unbound = attemptId === undefined ? undefined : and(
    eq(ingressDoorsAttempt.id, attemptId),
    eq(ingressDoorsAttempt.state, "pending"),
    or(isNull(ingressDoorsAttempt.inngestRunId), eq(ingressDoorsAttempt.inngestRunId, runId)),
  );
  const rows = yield* drizzle.update(ingressDoorsAttempt).set({
    state: "failed",
    failure: sql<string>`case when ${ingressDoorsAttempt.state} = 'pending' then ${pending} else ${running} end`,
    endedAt: new Date(),
  }).where(or(and(eq(ingressDoorsAttempt.inngestRunId, runId), inArray(ingressDoorsAttempt.state, ACTIVE_STATES)), unbound))
    .returning({ id: ingressDoorsAttempt.id });
  return rows.length;
});

/** The hourly sweep: a request no run picked up in time never starts; a switch running past its limit lost its run. */
export const closeStaleDoors = Effect.fn("IngressDoors.closeStale")(function* () {
  const { drizzle } = yield* Database;
  const now = Date.now();
  const lost = yield* drizzle.update(ingressDoorsAttempt).set(failed(RUN_ENDS.failure.running))
    .where(and(eq(ingressDoorsAttempt.state, "running"), lt(ingressDoorsAttempt.startedAt, new Date(now - 2 * DOORS_RUNNING_LIMIT_MS))))
    .returning({ id: ingressDoorsAttempt.id });
  const unstarted = yield* drizzle.update(ingressDoorsAttempt).set(failed(RUN_ENDS.failure.pending))
    .where(and(eq(ingressDoorsAttempt.state, "pending"), lt(ingressDoorsAttempt.requestedAt, new Date(now - DOORS_PENDING_LIMIT_MS))))
    .returning({ id: ingressDoorsAttempt.id });
  return { unstarted: unstarted.length, lost: lost.length };
});
