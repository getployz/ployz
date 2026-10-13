import "@tanstack/react-start/server-only";
import type { EnvironmentRef, MachineId, Snapshot, VolumeCopy } from "@ployz/sdk";
import { and, eq, inArray, max } from "drizzle-orm";
import { Effect } from "effect";
import { volumePlacement } from "#/modules/volume-placement/tables";
import { deployClaim } from "#/modules/volume-placement/volume-placement.server";
import { expectOf, roleOf } from "#/modules/volume-run/plan";
import { volumeMirrorStatus, volumeRun } from "#/modules/volume-run/tables";
import { ACTIVE_VOLUME_RUN_STATES, type AnsweredMember, copyName, type Member } from "#/modules/volume-run/volume-run";
import { askCopy, DRAIN_MS, observeVolumeMembers, requestVolumeRun } from "#/modules/volume-run/volume-run.server";
import { Database } from "#/server/database.server";

export type SyncedVolume = {
  readonly volumeId: string;
  readonly name: string;
  readonly dockerVolume: string;
  readonly environment: EnvironmentRef;
};

/** What one sync did with one mirror. */
export type MirrorStep =
  | { readonly kind: "handed_off"; readonly mirror: string; readonly target: string }
  | { readonly kind: "pulling"; readonly mirror: string; readonly target: string }
  | { readonly kind: "current"; readonly mirror: string; readonly snapshot: string }
  | { readonly kind: "refused"; readonly mirror: string; readonly message: string };

/** What one sync of one Volume did. A sync never opens a lane except to demote a primary that came back. */
export type MirrorSync =
  | { readonly kind: "covered"; readonly run: { readonly id: string; readonly kind: string } }
  | { readonly kind: "deploying"; readonly message: string }
  | { readonly kind: "unanswered"; readonly servers: readonly string[] }
  | { readonly kind: "no_mirror" }
  | { readonly kind: "no_writer" }
  | { readonly kind: "cutting"; readonly server: string }
  | { readonly kind: "demoting"; readonly run: string }
  | { readonly kind: "refused"; readonly message: string }
  | { readonly kind: "synced"; readonly mirrors: readonly MirrorStep[] };

export type PullFailure = { readonly mirror: string; readonly target: string; readonly failure: string };

export type MirrorSyncAnswer = { readonly reported: readonly PullFailure[]; readonly sync: MirrorSync; readonly lines: readonly string[] };

export type MemberObserver = (organizationId: string, dockerVolume: string) => ReturnType<typeof observeVolumeMembers>;
export type CopyAsk = typeof askCopy;

type Slot = Extract<VolumeCopy, { kind: "slot" }>;
type SlotMember = AnsweredMember & { readonly view: { readonly copy: Slot } };

const isSlot = (member: Member): member is SlotMember => member.answered && member.view.copy?.kind === "slot";
const pullingTo = (copy: Slot) => copy.pull !== null && copy.pull.failure === null ? copy.pull.target : null;

/** One line per thing the sync did or did not do, for the log and for whoever asked. */
export function syncLines(name: string, { reported, sync }: Omit<MirrorSyncAnswer, "lines">): string[] {
  const failures = reported.map(({ mirror, target, failure }) => `${mirror} could not receive ${target}: ${failure}`);
  return [...failures, ...outcomeLines(name, sync)];
}

function outcomeLines(name: string, sync: MirrorSync): string[] {
  switch (sync.kind) {
    case "covered":
      return [sync.run.kind === "move"
        ? `${name} refresh is covered by move run ${sync.run.id}`
        : `skipped ${name}, ${sync.run.kind} run ${sync.run.id} is open`];
    case "deploying":
      return [`skipped ${name}: ${sync.message}`];
    case "unanswered":
      return [`skipped ${name}, ${sync.servers.join(", ")} ${sync.servers.length === 1 ? "isn't" : "aren't"} answering`];
    case "no_mirror":
      return [`${name} has no mirror`];
    case "no_writer":
      return [`skipped ${name}, it has no writer`];
    case "cutting":
      return [`skipped ${name}, its writer on ${sync.server} is mid-move; volume release ${name} ends it`];
    case "demoting":
      return [`${name} has two primaries; run ${sync.run} makes the older one read-only`];
    case "refused":
      return [`skipped ${name}: ${sync.message}`];
    case "synced":
      return sync.mirrors.map((step) => {
        switch (step.kind) {
          case "handed_off":
            return `${step.mirror} is pulling ${step.target}`;
          case "pulling":
            return `still pulling ${step.target} on ${step.mirror}`;
          case "current":
            return `${step.mirror} already holds ${step.snapshot}`;
          case "refused":
            return `${step.mirror} refused the pull: ${step.message}`;
        }
      });
  }
}

const recordFailure = Effect.fn("MirrorSync.recordFailure")(function* (organizationId: string, volumeId: string, machineId: MachineId, target: string, failure: string) {
  const { drizzle } = yield* Database;
  const now = new Date();
  yield* drizzle.insert(volumeMirrorStatus)
    .values({ organizationId, volumeId, machineId, failedAt: now, failedSnapshot: target, failure, updatedAt: now })
    .onConflictDoUpdate({
      target: [volumeMirrorStatus.volumeId, volumeMirrorStatus.machineId],
      set: { failedAt: now, failedSnapshot: target, failure, updatedAt: now },
    });
});

/** A pull that landed; `matched` when the mirror is at the writer's newest as of now. */
const recordSuccess = Effect.fn("MirrorSync.recordSuccess")(function* (
  organizationId: string, volumeId: string, machineId: MachineId, snapshot: string, matched: boolean,
) {
  const { drizzle } = yield* Database;
  const now = new Date();
  const set = matched
    ? { okAt: now, okSnapshot: snapshot, matchedAt: now, updatedAt: now }
    : { okAt: now, okSnapshot: snapshot, updatedAt: now };
  yield* drizzle.insert(volumeMirrorStatus)
    .values({ organizationId, volumeId, machineId, ...set })
    .onConflictDoUpdate({ target: [volumeMirrorStatus.volumeId, volumeMirrorStatus.machineId], set });
});

/**
 * The gen a rowless SetVolumeCopy carries: the Volume's newest lane's. With no lane open, every floor Cloud raised on
 * this Volume is at most that, and a lane opened since raises the floor past it, so the Machine refuses the sync Stale.
 */
const newestGen = Effect.fn("MirrorSync.newestGen")(function* (volumeId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ gen: max(volumeRun.gen) }).from(volumeRun).where(eq(volumeRun.volumeId, volumeId));
  return row?.gen ?? 0;
});

/**
 * The one mirror sync: the 15-minute cron, a user's sync and Refresh now, and the refresh after a Deploy all run it.
 * It reports each failed pull once, before anything else, so an open lane never hides one. With a lane open it says
 * why it does nothing. Otherwise it snapshots the writer and hands each idle mirror the pull, without waiting for it.
 */
export const syncMirrors = Effect.fn("MirrorSync.sync")(function* (
  organizationId: string,
  volume: SyncedVolume,
  observe: MemberObserver = observeVolumeMembers,
  ask: CopyAsk = askCopy,
) {
  const { drizzle } = yield* Database;
  const answer = (reported: readonly PullFailure[], sync: MirrorSync) => {
    const lines = syncLines(volume.name, { reported, sync });
    return Effect.as(Effect.forEach(lines, (line) => Effect.logInfo(line, { organizationId, volumeId: volume.volumeId })), {
      reported, sync, lines,
    } satisfies MirrorSyncAnswer);
  };

  const members = yield* observe(organizationId, volume.dockerVolume);
  const [placement] = yield* drizzle.select().from(volumePlacement)
    .where(and(eq(volumePlacement.organizationId, organizationId), eq(volumePlacement.volumeId, volume.volumeId)));
  const slots = members.filter(isSlot);
  const mirrorIds = placement?.mirrors ?? slots.map((member) => member.machine.id);
  const mirrors = slots.filter((member) => mirrorIds.includes(member.machine.id));

  const statuses = mirrors.length === 0 ? [] : yield* drizzle.select().from(volumeMirrorStatus).where(and(
    eq(volumeMirrorStatus.volumeId, volume.volumeId),
    inArray(volumeMirrorStatus.machineId, mirrors.map((member) => member.machine.id)),
  ));
  const reported: PullFailure[] = [];
  for (const member of mirrors) {
    const { pull } = member.view.copy;
    if (pull?.failure == null) continue;
    const status = statuses.find((row) => row.machineId === member.machine.id);
    if (status?.failedSnapshot === pull.target && status.failure === pull.failure) continue;
    yield* recordFailure(organizationId, volume.volumeId, member.machine.id, pull.target, pull.failure);
    reported.push({ mirror: copyName(volume.name, member.machine.name), target: pull.target, failure: pull.failure });
  }

  // Read before the lane check, so a lane opened after that check carries a higher gen and fences this sync.
  const gen = yield* newestGen(volume.volumeId);
  const [lane] = yield* drizzle.select({ id: volumeRun.id, kind: volumeRun.kind }).from(volumeRun)
    .where(and(eq(volumeRun.volumeId, volume.volumeId), inArray(volumeRun.state, [...ACTIVE_VOLUME_RUN_STATES]))).limit(1);
  if (lane !== undefined) return yield* answer(reported, { kind: "covered", run: lane });
  const claim = yield* deployClaim(organizationId, placement);
  if (claim !== null) return yield* answer(reported, { kind: "deploying", message: claim.message });

  const silent = members.flatMap((member) => member.answered ? [] : [member.machine.name]);
  if (silent.length > 0) return yield* answer(reported, { kind: "unanswered", servers: silent });
  const answered = members.filter((member): member is AnsweredMember => member.answered);
  const roots = answered.filter((member) => roleOf(member) === "writer" || roleOf(member) === "cutting");
  if (roots.length > 1) {
    const demote = yield* requestVolumeRun({ userId: null, organizationId }, {
      volumeId: volume.volumeId, environment: volume.environment, kind: "sync", args: { full: false },
    });
    return yield* answer(reported, demote.ok ? { kind: "demoting", run: demote.run.id } : { kind: "refused", message: demote.refusal.message });
  }
  if (mirrors.length === 0) return yield* answer(reported, { kind: "no_mirror" });
  const [writer] = roots;
  if (writer?.view.copy?.kind !== "root") return yield* answer(reported, { kind: "no_writer" });
  if (roleOf(writer) === "cutting") return yield* answer(reported, { kind: "cutting", server: writer.machine.name });
  const epoch = writer.view.copy.epoch;

  const asker = { organizationId, dockerVolume: volume.dockerVolume, volumeName: volume.name, gen };
  const steps: MirrorStep[] = [];
  const named = (member: Member) => copyName(volume.name, member.machine.name);

  // The Machine finishes a landed pull only when asked for it again, so resend each pull before calling it busy.
  const busy: MirrorStep[] = [];
  const idle: { readonly member: SlotMember; readonly copy: Slot }[] = [];
  for (const member of mirrors) {
    const { copy } = member.view;
    const target = pullingTo(copy);
    if (target === null) {
      idle.push({ member, copy });
      continue;
    }
    const resent = yield* ask(asker, member.machine, expectOf(copy), { kind: "mirror", from: writer.address, target, drain: DRAIN_MS });
    if (resent.ok && resent.copy?.kind === "slot" && resent.copy.pull === null) idle.push({ member, copy: resent.copy });
    else if (resent.ok || resent.reason === "busy") busy.push({ kind: "pulling", mirror: named(member), target });
    else busy.push({ kind: "refused", mirror: named(member), message: resent.message });
  }
  // A writer keeps its newest two snapshots, so a new one while a pull runs could prune that pull's source.
  if (busy.length > 0) return yield* answer(reported, { kind: "synced", mirrors: busy });

  const snapshot = yield* ask(asker, writer.machine, { kind: "writer", epoch }, { kind: "snapshot", name: `w-${epoch}-${Math.floor(Date.now() / 1000)}` });
  if (!snapshot.ok) return yield* answer(reported, { kind: "refused", message: snapshot.message });
  const target: Snapshot | null = snapshot.copy?.newest ?? null;
  if (target === null) return yield* answer(reported, { kind: "refused", message: `${named(writer)} made no snapshot` });

  for (const { member, copy } of idle) {
    if (copy.pull === null && copy.newest?.guid === target.guid) {
      yield* recordSuccess(organizationId, volume.volumeId, member.machine.id, target.name, true);
      steps.push({ kind: "current", mirror: named(member), snapshot: target.name });
      continue;
    }
    if (copy.pull === null && copy.newest !== null) {
      const status = statuses.find((row) => row.machineId === member.machine.id);
      if (status?.okSnapshot !== copy.newest.name) yield* recordSuccess(organizationId, volume.volumeId, member.machine.id, copy.newest.name, false);
    }
    const handed = yield* ask(asker, member.machine, expectOf(copy), { kind: "mirror", from: writer.address, target: target.name, drain: DRAIN_MS });
    if (handed.ok) {
      steps.push({ kind: "handed_off", mirror: named(member), target: target.name });
    } else if (handed.reason === "busy") {
      steps.push({ kind: "pulling", mirror: named(member), target: handed.copy?.kind === "slot" ? handed.copy.pull?.target ?? target.name : target.name });
    } else {
      steps.push({ kind: "refused", mirror: named(member), message: handed.message });
    }
  }
  return yield* answer(reported, { kind: "synced", mirrors: steps });
});
