import "@tanstack/react-start/server-only";
import type { ContainerId, DrainReport, EnvironmentRef, MachineId, MachineRef, QualifiedService, RuntimeWatchView, ServiceDrain } from "@ployz/sdk";
import { and, eq, getTableColumns, inArray, isNotNull, ne, or, sql } from "drizzle-orm";
import { Effect, Option } from "effect";
import type { StoreRefusal } from "#/lib/refusal";
import { readStore } from "#/modules/config-store/config-store.server";
import { attemptRows } from "#/modules/inngest/attempt-row.server";
import { checkedNow } from "#/db/tables";
import { DRAIN_RUNNING_LIMIT_MS } from "#/modules/machines/server-drain";
import { candidateServers, copyHolder, dataCheck, type DrainRunRow, finalReport, type FinishedMove, volumeLine, type VolumeLine } from "#/modules/machines/server-drain-volumes";
import { requirements, serverSupports } from "#/modules/runtime/daemon-compat";
import { serverDrainAttempt } from "#/modules/machines/tables";
import { parseDockerVolumeName } from "#/modules/runtime/naming";
import { firstRuntimeFrame } from "#/modules/runtime/organization-runtime.server";
import { expectOf, planFromCopies } from "#/modules/volume-run/plan";
import { volumeRun } from "#/modules/volume-run/tables";
import { ACTIVE_VOLUME_RUN_STATES } from "#/modules/volume-run/volume-run";
import { observeVolumeMembers, readHolder, startRefusal } from "#/modules/volume-run/volume-run.cluster.server";
import { activeLane, admitGuardedRun, commitment, laneInputOf, namedOwedRefusal, readRow, releaseOfMove, resolveVolume, type ResolvedVolume, rowlessGen } from "#/modules/volume-run/volume-run.rows.server";
import { Database } from "#/server/database.server";
import { uuidFrom } from "#/server/uuid-from";

/**
 * A drain's Volume moves end before its row's running limit by this much, so the run ends its own row with what it saw
 * before the hourly sweep would.
 */
const DRAIN_VOLUME_MARGIN_MS = 30 * 60_000;

/** The one move a drain may admit for a Volume: a retried step finds it rather than admitting a second. */
export const drainMoveId = (attemptId: string, volumeId: string) => uuidFrom(`ployz.drain-move:${attemptId}:${volumeId}`);

/** Plain JSON, as a step returns it: one Volume the drain will try to move, and the Servers it may go to, best first. */
export type DrainVolume = {
  readonly service: QualifiedService;
  readonly volumeId: string;
  readonly volumeName: string;
  readonly environment: EnvironmentRef;
  readonly holder: ContainerId;
  readonly candidates: readonly MachineRef[];
  readonly run: string;
};

type Request = { readonly attemptId: string; readonly organizationId: string; readonly machineId: MachineId };
type Note = Pick<ServiceDrain, "volume" | "detail" | "run">;

const rows = attemptRows(serverDrainAttempt, "The drain request was not found.");

/**
 * How long a drain may still wait on its Volumes: from its start, which Postgres stamped, to the run limit less a
 * margin, on Postgres's clock as the statement reads it.
 */
const volumeLeftMs = sql<number>`coalesce(extract(epoch from ${serverDrainAttempt.startedAt}
  + make_interval(secs => ${(DRAIN_RUNNING_LIMIT_MS - DRAIN_VOLUME_MARGIN_MS) / 1000}) - ${checkedNow()}) * 1000, 0)`.mapWith(Number);

/**
 * The drain's row while this run owns it running with the Engine's report; null once it ended or another run holds it.
 * With `lock`, it locks the row first and reads it in the next statement, so the time left counts any wait for the lock.
 */
const ownedReport = Effect.fn("ServerDrain.ownedReport")(function* (request: Request, runId: string, lock = false) {
  const { drizzle } = yield* Database;
  if (lock) yield* drizzle.select({ id: serverDrainAttempt.id }).from(serverDrainAttempt).where(eq(serverDrainAttempt.id, request.attemptId)).for("update");
  const [row] = yield* drizzle.select({ ...getTableColumns(serverDrainAttempt), leftMs: volumeLeftMs }).from(serverDrainAttempt).where(and(
    rows.owned(request.attemptId, runId),
    eq(serverDrainAttempt.state, "running"),
    isNotNull(serverDrainAttempt.report),
  ));
  return row === undefined || row.report === null ? null : { ...row, report: row.report };
});

/** Writes `notes` onto the report's entries, while this run still owns the row running. */
const noteReport = Effect.fn("ServerDrain.noteReport")(function* (request: Request, runId: string, notes: ReadonlyMap<QualifiedService, Note>) {
  if (notes.size === 0) return;
  const { drizzle } = yield* Database;
  const row = yield* ownedReport(request, runId, true);
  if (row === null) return;
  const services = row.report.services.map((entry) => {
    const note = notes.get(entry.service);
    return note === undefined ? entry : { ...entry, ...note };
  });
  yield* drizzle.update(serverDrainAttempt).set({ report: { ...row.report, services } })
    .where(and(rows.owned(request.attemptId, runId), eq(serverDrainAttempt.state, "running")));
});

const stay = (volume: string | undefined, detail: string): Note => ({ volume, detail, run: undefined });

const readFrame = (organizationId: string) => firstRuntimeFrame(organizationId).pipe(
  Effect.flatMap((frame) => frame === null ? Effect.fail(new Error("Cloud couldn't read the cluster to plan the drain's volumes.")) : Effect.succeed(frame)),
);

/** The Servers a Cloud Drain is pending or running on, which no Volume moves to. */
const drainingServers = Effect.fn("ServerDrain.drainingServers")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const active = yield* drizzle.select({ machineId: serverDrainAttempt.machineId }).from(serverDrainAttempt)
    .where(and(eq(serverDrainAttempt.organizationId, organizationId), rows.active));
  return new Set<MachineId>(active.map((row) => row.machineId));
});

/**
 * The Services the Engine left on the drained Server for a Volume there, sorted: each that Cloud may move with its one
 * Managed volume, and the Servers it may go to; the rest get a named stay on the report. A folder or a Docker volume
 * keeps the Engine's own words. A stopped Engine report moves nothing.
 */
export const planDrainVolumes = Effect.fn("ServerDrain.planVolumes")(function* (request: Request, runId: string) {
  const none: DrainVolume[] = [];
  const row = yield* ownedReport(request, runId);
  if (row === null || row.report.stopped !== null) return none;
  const pinned = row.report.services.filter((entry) =>
    entry.result === "stays" && entry.reason.kind === "volume" && entry.reason.server.id === request.machineId);
  if (pinned.length === 0) return none;
  const frame = yield* readFrame(request.organizationId);
  const { namespaces } = yield* readStore(request.organizationId, { query: "namespaces" });
  const draining = yield* drainingServers(request.organizationId);
  // A drain's cut needs the Service still running when it stops it, which only a Server advertising it checks: an older
  // one would cut a Service its operator stopped since, and B would start it.
  const source = frame.machines.find(({ machine }) => machine.id === request.machineId)?.machine;
  const cutsRunning = source !== undefined && serverSupports({ capabilities: source.runtime.capabilities ?? [] }, requirements.volumeCutRunning) === true;
  const listings = new Map<string, Effect.Success<ReturnType<typeof listVolumes>>>();
  const volumes: DrainVolume[] = [];
  const notes = new Map<QualifiedService, Note>();
  for (const entry of pinned) {
    const check = dataCheck(frame, entry.service, request.machineId);
    // A folder or a Docker volume keeps the Engine's own words.
    if (check.kind === "engine") {
      notes.set(entry.service, { volume: undefined, detail: undefined, run: undefined });
      continue;
    }
    if (check.kind === "stays") {
      notes.set(entry.service, stay(undefined, check.why));
      continue;
    }
    const parsed = parseDockerVolumeName(check.dockerVolume);
    const owned = parsed === null ? undefined : namespaces.find((namespace) => namespace.namespace === parsed.namespace);
    if (parsed === null || owned === undefined) {
      notes.set(entry.service, stay(undefined, "no Environment owns its volume"));
      continue;
    }
    const environment = { project: owned.project, environment: owned.environment };
    const key = `${owned.project}/${owned.environment}`;
    const listed = listings.get(key) ?? (yield* listVolumes(request.organizationId, environment));
    listings.set(key, listed);
    const volume = listed.find((candidate) => candidate.id === parsed.volumeId);
    if (volume === undefined) {
      notes.set(entry.service, stay(undefined, "its volume is no longer in its Environment"));
      continue;
    }
    if (!cutsRunning) {
      notes.set(entry.service, stay(volume.name, `Upgrade ${row.report.server.name} to move ${volume.name} with the drain`));
      continue;
    }
    const refused = yield* volumeBusy(volume.id, volume.name, volume.shared_writes === true);
    if (refused !== null) {
      notes.set(entry.service, stay(volume.name, refused));
      continue;
    }
    const holder = frame.containers.find((container) => container.container_id === check.holder);
    const candidates = candidateServers(frame, {
      from: request.machineId,
      regions: Object.keys(holder?.resolved_spec.regions ?? {}),
      copy: copyHolder(frame, check.dockerVolume),
      draining,
    });
    if (candidates.length === 0) {
      notes.set(entry.service, stay(volume.name, "no other server can take it: none that answers, takes services and has managed storage in its region"));
      continue;
    }
    const run = drainMoveId(request.attemptId, volume.id);
    volumes.push({ service: entry.service, volumeId: volume.id, volumeName: volume.name, environment, holder: check.holder, candidates, run });
    notes.set(entry.service, { volume: volume.name, detail: undefined, run });
  }
  yield* noteReport(request, runId, notes);
  return volumes;
});

const listVolumes = (organizationId: string, environment: EnvironmentRef) =>
  readStore(organizationId, { query: "volumes", environment }).pipe(Effect.map(({ volumes }) => volumes));

/** Why a Volume can't move now, before any Server is asked: it allows shared writes, or another run holds it. */
const volumeBusy = Effect.fn("ServerDrain.volumeBusy")(function* (volumeId: string, name: string, shared: boolean) {
  if (shared) return `${name} allows shared writes, and a move takes a volume one service writes`;
  const lane = yield* activeLane(volumeId);
  if (lane !== undefined) return `${name} is already busy, run ${lane.id}`;
  const { owed } = yield* commitment(volumeId);
  if (owed !== null) return `${name} is still finishing an earlier move, run ${owed.id}`;
  return null;
});

/**
 * Why moving `item` to `to` would be refused, asking only what the run asks before it stops anything: the plan its
 * copies give, the Service on `from` and `to`'s check of the start. Null when `to` can take it. Changes nothing.
 */
const moveRefusal = Effect.fn("ServerDrain.moveRefusal")(function* (
  organizationId: string,
  volume: ResolvedVolume,
  item: DrainVolume,
  from: MachineRef,
  to: MachineRef,
  gen: number,
) {
  const args = { to: to.name, machine: to.id };
  const members = yield* observeVolumeMembers(organizationId, volume.dockerVolume, { kind: "move", args });
  const planned = planFromCopies({ kind: "move", args, volumeName: volume.volumeName, orphan: false }, members);
  if (!planned.ok) return planned.refusal.message;
  const { phase } = planned;
  if (phase.kind !== "move" || phase.writer.machine.id !== from.id || phase.start !== "rounds") {
    return `${volume.volumeName}'s writer isn't settled on ${from.name}`;
  }
  const asker = { organizationId, dockerVolume: volume.dockerVolume, volumeName: volume.volumeName, gen, volumeId: item.volumeId, recordedBytes: volume.recordedBytes };
  const holder = yield* readHolder(asker, from);
  if (!holder.ok) return holder.refusal.message;
  if (holder.holder.containerId !== item.holder) return `${holder.holder.redactedSpec.name} changed on ${from.name} since the drain looked`;
  return yield* startRefusal(asker, from, to, expectOf(phase.target.view.copy), item.holder);
});

/**
 * The parent checks a drain's move is admitted under, in its insert's transaction: the drain still runs under this run
 * with its report and before its deadline, and nothing drains the destination.
 */
const drainStillOwns = (request: Request, runId: string, to: MachineRef) => Effect.gen(function* () {
  const row = yield* ownedReport(request, runId, true);
  const refuse = (message: string): StoreRefusal => ({ code: "conflict", message, details: null });
  if (row === null) return refuse("the drain ended before it moved this volume");
  if (row.leftMs <= 0) return refuse("the drain ran out of time before it moved this volume");
  const { drizzle } = yield* Database;
  const [other] = yield* drizzle.select({ id: serverDrainAttempt.id }).from(serverDrainAttempt).where(and(
    eq(serverDrainAttempt.organizationId, request.organizationId),
    eq(serverDrainAttempt.machineId, to.id),
    rows.active,
  )).limit(1);
  return other === undefined ? null : refuse(`${to.name} is being drained`);
});

/**
 * The drain's one move for `item`: the first Server that passes the read-only checks gets it, recording the drained
 * Server and the Container that runs the Service there. A retried step finds the move it admitted. When no Server
 * passes, or the admission refuses, the Volume stays with that reason on the report.
 */
export const admitDrainVolume = Effect.fn("ServerDrain.admitVolume")(function* (request: Request, runId: string, item: DrainVolume) {
  if ((yield* readRow(item.run)) !== undefined) return { admitted: true } as const;
  const row = yield* ownedReport(request, runId);
  if (row === null) return { admitted: false } as const;
  const from: MachineRef = { id: request.machineId, name: row.report.server.name };
  const named = (detail: string) =>
    noteReport(request, runId, new Map([[item.service, stay(item.volumeName, detail)]])).pipe(Effect.as({ admitted: false } as const));
  const volume = yield* resolveVolume(request.organizationId, item.environment, item.volumeId).pipe(Effect.option);
  if (Option.isNone(volume)) return yield* named("its volume is no longer in its Environment");
  const gen = yield* rowlessGen(item.volumeId);
  let last = "no server could take it";
  let to: MachineRef | null = null;
  for (const candidate of item.candidates) {
    const refusal = yield* moveRefusal(request.organizationId, volume.value, item, from, candidate, gen);
    if (refusal === null) {
      to = candidate;
      break;
    }
    last = `${candidate.name}: ${refusal}`;
  }
  if (to === null) return yield* named(last);
  const target = to;
  // Only a Server that answers it takes services gets the move: B starts the Service without asking.
  const frame: RuntimeWatchView | null = yield* firstRuntimeFrame(request.organizationId).pipe(Effect.orElseSucceed(() => null));
  const listed = frame?.unanswered.includes(target.id) === false ? frame.machines.find(({ machine }) => machine.id === target.id)?.machine : undefined;
  if (listed === undefined) return yield* named(`${target.name} didn't answer whether it takes services`);
  if (!listed.accepts_services) return yield* named(`${target.name} stopped taking services`);
  const admitted = yield* admitGuardedRun(
    { userId: row.requestedByUserId, organizationId: request.organizationId },
    { volumeId: item.volumeId, kind: "move", args: { to: target.name, machine: target.id, holder: { machine: from.id, container: item.holder } } },
    volume.value,
    item.run,
    drainStillOwns(request, runId, target),
  );
  if (admitted.ok) return { admitted: true } as const;
  const refusal = admitted.owed === null ? admitted.refusal : yield* namedOwedRefusal(admitted.owed);
  return yield* named(refusal.message);
});

const asRunRow = (row: typeof volumeRun.$inferSelect | undefined): DrainRunRow | undefined => {
  if (row === undefined) return undefined;
  const input = laneInputOf(row);
  return {
    id: row.id,
    kind: row.kind,
    state: row.state,
    phase: row.phase,
    message: row.message,
    to: input.kind === "move" ? input.args.to : null,
    restarted: row.result?.restarted ?? null,
  };
};

const runRows = Effect.fn("ServerDrain.runRows")(function* (moves: readonly string[]) {
  if (moves.length === 0) return new Map<string, typeof volumeRun.$inferSelect>();
  const { drizzle } = yield* Database;
  const found = yield* drizzle.select().from(volumeRun).where(inArray(volumeRun.id, [...moves, ...moves.map(releaseOfMove)]));
  return new Map(found.map((row) => [row.id, row]));
});

/**
 * Where the drain's move of `item` stands, for its wait: the run it waits on next, the move first and then the Release
 * its end queued, or null once both ended. `leftMs` is how long the drain may still wait.
 */
export const drainMoveState = Effect.fn("ServerDrain.moveState")(function* (request: Request, runId: string, item: DrainVolume) {
  const row = yield* ownedReport(request, runId);
  const leftMs = row?.leftMs ?? 0;
  const found = yield* runRows([item.run]);
  const active = (id: string) => {
    const run = found.get(id);
    return run !== undefined && ACTIVE_VOLUME_RUN_STATES.some((state) => state === run.state);
  };
  const waitFor = active(item.run) ? item.run : active(releaseOfMove(item.run)) ? releaseOfMove(item.run) : null;
  return { waitFor, leftMs };
});

type RunRows = ReadonlyMap<string, typeof volumeRun.$inferSelect>;

/** The drain's moves that finished, by run, as `found` holds their rows and their Releases'. */
const finishedMoves = (report: DrainReport, found: RunRows) => {
  const finished = new Map<string, FinishedMove>();
  for (const entry of report.services) {
    const move = entry.run === undefined ? undefined : found.get(entry.run);
    if (entry.run === undefined || move === undefined) continue;
    // A committed move that failed is finished by its Release, forward on the new Server.
    const release = found.get(releaseOfMove(entry.run));
    const finisher = move.state === "done" ? move : move.phase === "committed" && release?.state === "done" ? release : undefined;
    const input = laneInputOf(move);
    if (finisher === undefined || input.kind !== "move") continue;
    const to: MachineRef = { id: move.current ?? input.args.machine ?? report.server.id, name: input.args.to };
    // Only a Container a Server acknowledged removing is gone; one a frame missed stays where it was.
    const removed = [...(move.result?.removed ?? []), ...(release?.result?.removed ?? [])];
    finished.set(entry.run, { to, removed });
  }
  return finished;
};

const reportRuns = (report: DrainReport) => runRows(report.services.flatMap((entry) => entry.run ?? []));

/** The report with what its moves did since the drain stopped waiting on them, as `found` holds them, read against a fresh frame. */
const settledReport = Effect.fn("ServerDrain.settledReport")(function* (organizationId: string, report: DrainReport, found: RunRows) {
  const finished = finishedMoves(report, found);
  if (!report.services.some((entry) => entry.run !== undefined && finished.has(entry.run) && entry.result !== "moved")) return report;
  return finalReport(report, finished, yield* freshFrame(organizationId));
});

const freshFrame = (organizationId: string) =>
  firstRuntimeFrame(organizationId).pipe(Effect.orElseSucceed((): RuntimeWatchView | null => null));

/**
 * Each Volume entry's line, read from its move and that move's Release now, so a line stays current after the drain
 * ended. A drain that ended while a move it admitted ran on reads that move as the drain's own finish would. Returns
 * the report with each line as its entry's `detail`, and the lines by Service for the Server page.
 */
export const readVolumeLines = Effect.fn("ServerDrain.readVolumeLines")(function* (organizationId: string, ended: DrainReport, drainEnded: boolean) {
  const lines: Record<string, VolumeLine> = {};
  if (!ended.services.some((entry) => entry.volume !== undefined || entry.detail !== undefined)) return { report: ended, lines };
  // One read of the rows, so the report's outcome and its lines say the same of each move.
  const found = yield* reportRuns(ended);
  const report = drainEnded ? yield* settledReport(organizationId, ended, found) : ended;
  const services = report.services.map((entry) => {
    const run = entry.run;
    const line = volumeLine({
      entry,
      server: report.server,
      move: run === undefined ? undefined : asRunRow(found.get(run)),
      release: run === undefined ? undefined : asRunRow(found.get(releaseOfMove(run))),
      drainEnded,
    });
    if (line === null) return entry;
    lines[entry.service] = line;
    return { ...entry, detail: line.text };
  });
  return { report: { ...report, services }, lines };
});

/** The drain's last step: the report as its finished moves leave it (see `finalReport`). Ends the row finished. */
export const finishDrain = Effect.fn("ServerDrain.finish")(function* (request: Request, runId: string) {
  const row = yield* ownedReport(request, runId);
  if (row === null) return { attemptId: request.attemptId, state: yield* rows.stateOf(request.attemptId) };
  const finished = finishedMoves(row.report, yield* reportRuns(row.report));
  // With nothing moved, the Engine's own observation of what remains still holds.
  const final = finished.size === 0 ? null : finalReport(row.report, finished, yield* freshFrame(request.organizationId));
  const state = yield* rows.end(request.attemptId, runId, { state: "finished", endedAt: sql`now()`, ...(final !== null && { report: final }) }, ["running"]);
  return { attemptId: request.attemptId, state: state ?? (yield* rows.stateOf(request.attemptId)) };
});

/**
 * A Volume Run that will start a Service on `machineId`: an active move there, or a committed move or release still owed
 * its start there. While one does, nothing turns services off there or drains it. A move admitted before moves recorded
 * their Server's id names it only by name until its run resolves and pins it, so until then it counts for every
 * Server: a name Cloud knows may be one the Server has since dropped.
 */
export const volumeRunTargeting = Effect.fn("ServerDrain.volumeRunTargeting")(function* (organizationId: string, machineId: MachineId) {
  const { drizzle } = yield* Database;
  const candidates = yield* drizzle.select().from(volumeRun).where(and(
    eq(volumeRun.organizationId, organizationId),
    inArray(volumeRun.kind, ["move", "release"]),
    or(
      inArray(volumeRun.state, ACTIVE_VOLUME_RUN_STATES),
      and(eq(volumeRun.phase, "committed"), eq(volumeRun.current, machineId), ne(volumeRun.state, "done")),
    ),
  ));
  for (const row of candidates) {
    const input = laneInputOf(row);
    const active = ACTIVE_VOLUME_RUN_STATES.some((state) => state === row.state);
    if (active && input.kind === "move" && (input.args.machine === undefined || input.args.machine === machineId)) return row;
    if (active && row.current === machineId) return row;
    if (row.phase === "committed" && row.current === machineId && (yield* commitment(row.volumeId)).owed?.id === row.id) return row;
  }
  return null;
});

export const targetedMessage = (row: typeof volumeRun.$inferSelect) => {
  const input = laneInputOf(row);
  const to = input.kind === "move" && input.args.machine === undefined ? `${input.args.to}, which may be this server` : "this server";
  return `${row.volumeName} is moving to ${to}. Wait for run ${row.id} to finish.`;
};
