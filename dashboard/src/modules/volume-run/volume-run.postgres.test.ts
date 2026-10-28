import type {
  ConfigStore,
  ContainerId,
  CopyObservation,
  CopyRefusal,
  CopyRotation,
  ExpectedCopy,
  MachineId,
  RuntimeWatchView,
  SetVolumeCopyReply,
  SetVolumeCopyRequest,
  SetVolumeSizeReply,
  SetVolumeSizeRequest,
  SlotRole,
  Snapshot,
  SystemEvent,
  VolumeCopy,
  VolumeCopyView,
  VolumeSwitchRequest,
  WantedCopy,
} from "@ployz/sdk";
import { RpcError } from "@ployz/sdk";
import { createHash } from "node:crypto";
import { InngestTestEngine, mockCtx } from "@inngest/test";
import { Effect, Exit } from "effect";
import { Inngest, NonRetriableError } from "inngest";
import type { PoolClient } from "pg";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { InngestClient } from "#/modules/inngest/client";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { requirements, ServerLacks } from "#/modules/runtime/daemon-compat";
import { firstFrame, PloyzProviderError, type PloyzSdkError, type PloyzSession } from "#/modules/runtime/ployz.server";
import { createCancelStoreDeployment, createRunStoreDeployment } from "#/modules/config-store/store-deployment.inngest";
import type { InngestRunStatus } from "#/modules/inngest/client";
import { configDeploymentAdmittedEvent } from "#/modules/inngest/events";
import { createCloseFinishedVolumeRun, createCloseStaleVolumeRuns, createRunVolume } from "#/modules/volume-run/volume-run.inngest";
import { operationsOn } from "#/modules/operations/operation-history.server";
import {
  askCopy,
  closeStaleVolumeRuns,
  closeVolumeRun,
  commitAndStart,
  commitMove,
  type CopyAnswer,
  insertRun,
  isVolumeOwed,
  listVolumeRuns,
  observeOrganization,
  observeOrganizationCopies,
  orphanSlotNames,
  planVolumeRun,
  releaseLanesOff,
  requestVolumeRun,
  requireOwner,
  resolveVolume,
  type RunContext,
  sendSwitch,
  setVolumeCopy,
  startOrphanDeletes,
  volumeBusyMessage,
} from "#/modules/volume-run/volume-run.server";
import { PATIENCE_WAITS } from "#/modules/volume-run/patience";
import type { Member, VolumeRunInput } from "#/modules/volume-run/volume-run";
import { member } from "#/modules/volume-run/volume-run.test-fixture";
import { appliedMirrors, observePlacement } from "#/modules/volume-placement/volume-placement";
import { deleteOldCopy } from "#/modules/volume-placement/volume-placement.server";
import { type DeployLane, fenceDeployVolume, finishDeployVolumes, placeDeployVolumes, releaseFollowed } from "#/modules/volume-run/volume-run.deploy.server";
import type { Caller } from "#/modules/identity/actor";
import { runVolume } from "#/modules/operations/operations.server";
import { decideApproval } from "#/modules/approvals/approvals.server";
import { decideAndApply } from "#/modules/approvals/apply.server";
import { FULL_GRANT } from "#/modules/identity/token-grant";
import type { Database } from "#/server/database.server";
import { makeInngestEffectRunner, type runInngestEffect } from "#/server/run.server";
import { invokeOnFailure } from "#/test/inngest-failure";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const organizationId = "00000000-0000-4000-8000-000000000e01";
const userId = "00000000-0000-4000-8000-000000000e02";
const volumeId = "vol1";
const dockerVolume = `shop-production_vol-${volumeId}`;
const environment = { project: "shop", environment: "production" };

const inngest = new Inngest({ id: "volume-run-test" });
const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

type PullFault = { target: string; kind: "fail" | "restart" | "stall" };

type FakeMachine = {
  name: string;
  /** A renamed Server keeps its id; the default derives it from the name. */
  id?: MachineId;
  pool: boolean;
  stateless?: boolean;
  silent?: boolean;
  lacks?: string;
  capabilities?: string[];
  copy: VolumeCopy | null;
  /** Writes since the writer's newest snapshot: a Snapshot takes a new one only while this is above zero. */
  writes?: number;
  pullFault?: PullFault;
  /** A restart stopped the running receive; the next Mirror set resumes it. */
  stopped?: boolean;
  /** The gen floor every admitted set raises, as the daemon does; a set with a lower gen is stale. */
  floor?: number;
  /** The size its copy holds; only a set that allows a shrink lowers it, as a daemon serving the shrink does. */
  bound?: number;
  /** The smallest bound its data accepts; a shrink below it is refused `below_floor`, as the daemon's floor check does. */
  needs?: number;
  /** Size requests it answers `switching` to before it holds the size. */
  switching?: number;
};

type FakeContainer = { machine: string; id: string; running: boolean; created?: number; rotation?: CopyRotation; gen?: number };

/**
 * What a fake Machine does with the next request of one label: refuse, apply it and lose the reply, fail with its own error, or
 * hold it in flight until the test lands it.
 */
type Fault = CopyRefusal | "lost" | "down" | "held" | RawFault;

/** A Machine's error exactly as sent, for refusals whose copy differs from the fake's own. */
class RawFault {
  constructor(readonly cause: { code: string; message: string; details: unknown }) {}
}

/** A reply that never came back, as the SDK rejects it: its own timeout on a target Machine's RPC, which no Machine sent. */
const NO_REPLY = new RpcError({ code: "unavailable", message: "target Machine RPC timed out", details: null, unanswered: true });

/** An RPC error shaped as the SDK's RpcError. */
type RpcFault = Error & { code: string };
const rpcFault = (code: string, message: string): RpcFault => Object.assign(new Error(message), { code });

/** A runtime watch that never yields, or that fails its first read with the Cluster's RPC error. */
const answering = (fault: true | RpcFault): AsyncIterable<RuntimeWatchView> => ({
  [Symbol.asyncIterator]: () => ({ next: () => (fault === true ? new Promise(() => {}) : Promise.reject(fault)) }),
});

const at = 1_790_000_000;
const GB = 1_000_000_000;
const snap = (guid: string, name = `dep-${guid}`): Snapshot => ({ name, guid, created_unix_seconds: at });
const base = snap("10");
const seedCopy: VolumeCopy = { kind: "root", readonly: false, newest: null, epoch: 0, seed: true };
const writerCopy = (newest: Snapshot | null = base, epoch = 0, readonly = false): VolumeCopy =>
  ({ kind: "root", readonly, newest, epoch, seed: false });
const slotCopy = (newest: Snapshot | null, pull: { target: string; failure: string | null } | null = null, slot = "1", role: SlotRole = "mirror"): VolumeCopy =>
  ({ kind: "slot", slot, readonly: true, newest, resume_token: null, pull, role });
const idOf = (name: string) => name.padEnd(32, "0") as MachineId;
const first = <T>(items: readonly T[]): T => {
  const item = items[0];
  if (item === undefined) throw new Error("expected at least one item");
  return item;
};

const label = (fake: FakeMachine, want: WantedCopy) => {
  switch (want.kind) {
    case "writer":
      return `writer@${fake.name}(${want.start?.kind ?? "none"})`;
    case "mirror":
      return `${fake.copy?.kind === "root" ? "cut" : "pull"}@${fake.name}(${want.target})`;
    case "snapshot":
      return `snapshot@${fake.name}(${want.name})`;
    case "forget":
      return `forget@${fake.name}`;
    case "floor":
      return `floor@${fake.name}`;
  }
};

describe("volume runs", () => {
  let harness: PostgresTestHarness;
  let machines: FakeMachine[];
  let containers: FakeContainer[];
  let snapshots: Map<string, Snapshot>;
  let guids: number;
  /** Slot ids a fake Machine gave out: every slot it creates gets a fresh one, as a fresh ZFS dataset gets a fresh guid. */
  let slotIds: number;
  let verbs: string[];
  let failNext: Map<string, Fault>;
  let beforeSet: Map<string, () => void | Promise<void>>;
  /** Runs while a writer start reads the kept Container's spec, between the commit and the send. */
  let beforeStart: (() => Promise<void>) | null;
  /**
   * Fails a runtime frame read, counted from 1 in `frames`: true as a Server that never answers its first frame, an RPC
   * error as the Cluster answering with it.
   */
  let frameFault: ((read: number) => boolean | RpcFault) | null;
  let frames: number;
  /**
   * Fails a session open, counted from 1 in `opens`: an Error as Cloud failing on its own side, or a Cluster it cannot
   * reach.
   */
  let openFault: ((open: number) => Error | "unreachable" | null) | null;
  let opens: number;
  let sent: SetVolumeCopyRequest[];
  let sizes: SetVolumeSizeRequest[];
  /** The running row's message each time a Machine was asked for a size. */
  let sizing: (string | null)[];
  let held: Map<string, () => CopyRefusal | null>;
  /** The Project's name in the Store; a read of its Environment by another name is not found. */
  let storeProject: string;
  /** Runs once after the Store answers a namespaces read, as a write committed between that read and the next. */
  let afterNamespaces: (() => void) | null;
  let storeVolumes: Array<{ id: string; name: string; storage: { kind: "provisioned"; maximumBytes: number } | { kind: "docker" }; change?: "delete"; recorded_storage?: number }>;
  /** Every system event the Store recorded, and how it refuses the next one, if it does. */
  let storeEvents: SystemEvent[];
  let storeRefuse: ((event: SystemEvent) => InstanceType<typeof RpcError> | null) | null;
  let storeInFlight: { id: string; number: number | null }[];
  let storeLatest: number | null;
  /** Whether the Store reads production as Protected. */
  let storeProtected: boolean;
  /** The nodes every Deployment the Store reads back targets. */
  let deployNodes: Array<{ type: "volume" | "service"; id: string; name: string; outcome: "pending"; rows: [] }>;

  const machine = (target: string) => {
    const found = machines.find((entry) => (entry.id ?? idOf(entry.name)) === target);
    if (found === undefined) throw new Error(`no fake Machine ${target}`);
    return found;
  };
  const named = (name: string) => {
    const found = machines.find((entry) => entry.name === name);
    if (found === undefined) throw new Error(`no fake Machine ${name}`);
    return found;
  };
  const newSnap = (name: string) => {
    guids += 1;
    const made = snap(String(100 + guids), name);
    snapshots.set(name, made);
    return made;
  };

  const newSlot = () => {
    slotIds += 1;
    return String(1000 + slotIds);
  };

  const startOn = (fake: FakeMachine, want: Extract<WantedCopy, { kind: "writer" }>) => {
    const start = want.start;
    if (start === null) return;
    if (start.kind === "kept") {
      for (const container of containers) if (container.id === start.container_id) container.running = true;
      return;
    }
    const id = `created-${start.creation_key}`;
    const made = containers.find((container) => container.id === id);
    if (made === undefined) containers.push({ machine: fake.name, id, running: true });
    else made.running = true;
  };

  const atWant = (fake: FakeMachine, want: WantedCopy) => {
    const copy = fake.copy;
    switch (want.kind) {
      case "writer":
        return copy?.kind === "root" && !copy.readonly && copy.newest?.guid === want.at && copy.epoch === want.epoch
          && (want.start === null || containers.some((container) => container.machine === fake.name && container.running));
      case "mirror":
        return copy?.kind === "slot" && copy.pull === null && copy.newest?.name === want.target && (copy.role !== "old_copy" || want.from === null);
      case "snapshot":
        return copy?.kind === "root" && copy.newest?.name === want.name;
      case "forget":
        return copy === null;
    }
  };

  const matches = (copy: VolumeCopy | null, expect: ExpectedCopy) => {
    switch (expect.kind) {
      case "none":
        return copy === null || (copy.kind === "slot" && copy.newest === null);
      case "writer":
        return copy?.kind === "root" && copy.epoch === expect.epoch;
      case "mirror":
        return copy?.kind === "slot" && copy.newest?.guid === expect.newest && copy.slot === expect.slot && (expect.role === undefined || copy.role === expect.role);
    }
  };

  /**
   * SetVolumeCopy as the daemon answers it: at the wanted state is success, then a landed pull of the wanted target
   * finishes whatever the expect, and an old copy already at it becomes the mirror, else the expect, else the transition.
   * A receive lands on an inspect or a Mirror set and keeps `pull`; a resend for the running target changes nothing. A
   * cut leaves an old copy; a pull makes a mirror.
   */
  const transition = (fake: FakeMachine, expect: ExpectedCopy, want: WantedCopy): CopyRefusal | null => {
    if (want.kind === "floor" || atWant(fake, want)) return null;
    const copy = fake.copy;
    if (want.kind === "mirror" && copy?.kind === "slot" && copy.pull?.target === want.target && copy.newest?.name === want.target) {
      fake.copy = { ...copy, pull: null };
      return null;
    }
    if (want.kind === "mirror" && want.from !== null && copy?.kind === "slot" && copy.pull === null && copy.newest?.name === want.target) {
      fake.copy = { ...copy, role: "mirror" };
      return null;
    }
    if (!matches(fake.copy, expect)) return "mismatch";
    if (copy?.kind === "root") {
      switch (want.kind) {
        case "mirror":
          for (const container of containers) if (container.machine === fake.name) container.running = false;
          fake.copy = slotCopy(newSnap(want.target), null, newSlot(), "old_copy");
          return null;
        case "snapshot":
          if ((fake.writes ?? 0) > 0) {
            fake.writes = (fake.writes ?? 0) - 1;
            fake.copy = { ...copy, newest: newSnap(want.name) };
          }
          return null;
        case "forget":
          if (copy.seed) fake.copy = null;
          return null;
        case "writer":
          if (copy.epoch > want.epoch) return "no_transition";
          if (copy.newest?.guid !== want.at) return "behind";
          fake.copy = { ...copy, readonly: false, epoch: want.epoch };
          startOn(fake, want);
          return null;
      }
    }
    switch (want.kind) {
      case "mirror":
        if (want.from === null) return "no_transition";
        if (copy?.kind === "slot" && copy.pull?.target === want.target) return null;
        fake.copy = slotCopy(copy?.newest ?? null, { target: want.target, failure: null }, copy?.kind === "slot" ? copy.slot : newSlot());
        return null;
      case "forget":
        fake.copy = null;
        return null;
      case "writer":
        if (copy?.newest?.guid !== want.at) return "behind";
        fake.copy = writerCopy(copy.newest, want.epoch);
        startOn(fake, want);
        return null;
      default:
        return "no_transition";
    }
  };

  const failure = (fault: Fault, copy: VolumeCopy | null) => {
    if (fault === "down") return { code: "internal", message: "dataset is busy", details: null };
    if (fault instanceof RawFault) return fault.cause;
    return { code: fault === "busy" ? "unavailable" : "failed_precondition", message: fault, details: { reason: fault, copy } };
  };

  const receive = (fake: FakeMachine) => {
    const copy = fake.copy;
    const pull = copy?.kind === "slot" ? copy.pull : null;
    if (copy?.kind !== "slot" || pull === null || pull.failure !== null || fake.stopped === true) return;
    const fault = fake.pullFault;
    if (fault?.target === pull.target && fault.kind === "stall") return;
    if (fault?.target === pull.target && fault.kind === "fail") fake.copy = { ...copy, pull: { ...pull, failure: "disk full" } };
    else if (fault?.target === pull.target) {
      fake.pullFault = undefined;
      fake.stopped = true;
    } else fake.copy = { ...copy, newest: snapshots.get(pull.target) ?? copy.newest };
  };

  const inspect = (fake: FakeMachine): VolumeCopyView => {
    receive(fake);
    return { copy: fake.copy };
  };

  const session = asTestDouble<PloyzSession>()({
    watchFirstFrame: (timeoutMs: number) => {
      const fault = frameFault?.(++frames) ?? false;
      return fault !== false ? firstFrame(() => answering(fault), fault === true ? 1 : timeoutMs) : Effect.succeed({
      machines: machines.map((fake) => ({
        machine: { id: fake.id ?? idOf(fake.name), name: fake.name, public_key: Array.from({ length: 32 }, (_, index) => index), runtime: { daemon_version: fake.lacks ?? "", capabilities: fake.capabilities } },
        storage: fake.stateless === true ? { state: "stateless" } : fake.pool ? { state: "pool", size_bytes: 1, used_bytes: 0, free_bytes: 1 } : { state: "ready" },
      })),
      containers: containers.map((container) => ({
        machine_id: idOf(container.machine),
        kind: "service_container",
        container_id: container.id,
        namespace: "shop-production",
        resolved_spec: { name: "web", container: { pull_policy: "always", environment: {} }, volumes: [{ source: { kind: "provisioned", name: dockerVolume } }] },
        runtime: { state: container.running ? "running" : "exited" },
        rotation: container.rotation,
        created_at_unix_nanos: container.created ?? 0,
      })),
      volumes: machines.flatMap((fake) => fake.copy === null || fake.bound === undefined ? [] : [{
        id: { machine_id: fake.id ?? idOf(fake.name), name: dockerVolume },
        options: {},
        labels: {},
        storage: { kind: "provisioned", mountpoint: `/volumes/${dockerVolume}`, bound_bytes: fake.bound, used_bytes: 0 },
      }]),
      incomplete_ids: { machines: [] },
    });
    },
    inspectContainer: (target: string, container: string) => Effect.promise(async () => {
      await beforeStart?.();
      const { rotation, gen } = containers.find((entry) => entry.id === container) ?? {};
      return {
        container: {
          container_id: container,
          rotation,
          gen,
          resolved_spec: { name: "web", container: { pull_policy: "always", environment: { POSTGRES_PASSWORD: `secret-${machine(target).name}` } }, volumes: [{ source: { kind: "provisioned", name: dockerVolume, maximum_bytes: 5_000_000 } }] },
        },
      };
    }),
    copyContainerImage: (from: string, container: string, to: string) => Effect.sync(() => {
      verbs.push(`copy_image@${machine(from).name}->${machine(to).name}(${container})`);
    }),
    removeContainer: (target: string, container: string) => Effect.sync(() => {
      verbs.push(`remove@${machine(target).name}(${container})`);
      containers = containers.filter((entry) => entry.id !== container);
    }),
    volumeSwitch: (target: string, request: VolumeSwitchRequest) => Effect.suspend((): Effect.Effect<VolumeCopyView | SetVolumeCopyReply | SetVolumeSizeReply, PloyzSdkError> => {
      const fake = machine(target);
      if (fake.silent === true) return Effect.never;
      if (fake.lacks !== undefined) return Effect.fail(new ServerLacks({ machineId: target, capability: requirements.volumeMove.capability }));
      const fail = (fault: Fault) => Effect.fail(new PloyzProviderError({ operation: "volume switch", cause: failure(fault, fake.copy) }));
      const unanswered = Effect.fail(new PloyzProviderError({ operation: "volume switch", cause: NO_REPLY }));
      if (request.command === "inspect_volume_copy") {
        const fault = failNext.get(`inspect@${fake.name}`);
        if (fault !== undefined) {
          failNext.delete(`inspect@${fake.name}`);
          return fail(fault);
        }
        return Effect.succeed(inspect(fake));
      }
      if (request.command === "set_volume_size") {
        const { payload } = request;
        const asked = payload.maximum_bytes;
        verbs.push(`size@${fake.name}(${asked / GB})`);
        sizes.push(payload);
        if (fake.capabilities?.includes(requirements.volumeSize.capability) === false) {
          return Effect.fail(new ServerLacks({ machineId: target, capability: requirements.volumeSize.capability }));
        }
        if ((fake.switching ?? 0) > 0) {
          fake.switching = (fake.switching ?? 0) - 1;
          return fail(new RawFault({ code: "failed_precondition", message: "switching", details: { reason: "switching" } }));
        }
        return Effect.promise(() => harness.pool.query<{ message: string | null }>("select message from volume_run where state = 'running'")).pipe(
          Effect.flatMap(({ rows: running }) => {
            sizing.push(...running.map((row) => row.message));
            const fault = failNext.get(`size@${fake.name}`);
            failNext.delete(`size@${fake.name}`);
            if (fault !== undefined) return fail(fault);
            const bound = fake.bound ?? 0;
            const shrinks = payload.allow_shrink === true && fake.capabilities?.includes(requirements.volumeShrink.capability) !== false;
            if (asked < bound && !shrinks) return fail(new RawFault({ code: "failed_precondition", message: "shrink", details: { reason: "shrink", bound_bytes: bound } }));
            if (asked < bound && fake.needs !== undefined && asked < fake.needs) {
              return fail(new RawFault({ code: "failed_precondition", message: "below_floor", details: { reason: "below_floor", bound_bytes: bound, floor_bytes: fake.needs } }));
            }
            fake.bound = asked;
            return Effect.succeed({ bound_bytes: asked });
          }),
        );
      }
      if (request.command !== "set_volume_copy") return Effect.die(new Error(`the run sent ${request.command}`));
      const { expect: expected, want } = request.payload;
      const verb = label(fake, want);
      const key = verb.replace(/\(.*$/, "");
      sent.push(request.payload);
      verbs.push(verb);
      if (want.kind === "floor" && fake.capabilities?.includes(requirements.volumeFloor.capability) === false) {
        return Effect.fail(new ServerLacks({ machineId: target, capability: requirements.volumeFloor.capability }));
      }
      const hook = beforeSet.get(key);
      beforeSet.delete(key);
      return Effect.promise(async () => hook?.()).pipe(Effect.andThen((): Effect.Effect<VolumeCopyView, PloyzSdkError> => {
        const fault = failNext.get(key);
        failNext.delete(key);
        if (fault === "lost") {
          transition(fake, expected, want);
          return unanswered;
        }
        if (fault === "held") {
          held.set(key, () => transition(fake, expected, want));
          return unanswered;
        }
        if (fault !== undefined) return fail(fault);
        const gen = request.payload.gen ?? 0;
        if (gen < (fake.floor ?? 0)) return fail("stale");
        fake.floor = Math.max(fake.floor ?? 0, gen);
        if (want.kind === "mirror") {
          receive(fake);
          fake.stopped = false;
          const copy = fake.copy;
          // The daemon answers a failed receive once, and the next resend retries it. Only a request its planner lets
          // through gets the answer: a source, and the expect matching or naming this slot.
          const passes = copy?.kind === "slot" && want.from !== null
            && ((expected.kind === "mirror" && expected.slot === copy.slot) || matches(copy, expected));
          if (passes && copy.pull?.target === want.target && copy.pull.failure !== null) {
            fake.copy = { ...copy, pull: { ...copy.pull, failure: null } };
            return Effect.succeed({ copy });
          }
        }
        const refused = transition(fake, expected, want);
        return refused === null ? Effect.succeed({ copy: fake.copy }) : fail(refused);
      }));
    }),
  });

  const runEffect = makeInngestEffectRunner(<A, E>(operation: Effect.Effect<A, E, Database | OrganizationRuntime | InngestClient | CloudStore>) =>
    harness.runEffect(operation.pipe(
      Effect.provideService(InngestClient, inngest),
      Effect.provideService(CloudStore, {
        open: Effect.succeed(asTestDouble<ConfigStore>()({
          githubCancel: async () => [],
          abandonDeployment: async (id: string) => ({ id, status: "failed" }),
          system: async (_: string, event: SystemEvent) => {
            const refusal = storeRefuse?.(event) ?? null;
            if (refusal !== null) throw refusal;
            storeEvents.push(event);
            return { written: "edited" };
          },
          read: async (_: string, query: { query: string; id?: string; environment?: { project: string | null } }) => {
            if (query.query === "namespaces") {
              const answer = { namespaces: [{ namespace: "shop-production", project: storeProject, environment: "production" }] };
              const after = afterNamespaces;
              afterNamespaces = null;
              after?.();
              return answer;
            }
            if (query.query === "deployments") return { in_flight: storeInFlight, latest: storeLatest === null ? null : { number: storeLatest } };
            if (query.query === "environments") return { environments: [{ id: "env-1", name: "production", protected: storeProtected }] };
            if (query.query === "deployment") {
              return { id: query.id, status: "running", namespace: "shop-production", environment: { id: "env-1", project: storeProject, name: "production" }, nodes: deployNodes };
            }
            if (query.environment !== undefined && query.environment.project !== storeProject) {
              throw new RpcError({ code: "not_found", message: `No Project named ${query.environment.project}`, details: null });
            }
            return { environment: { id: "env-1", project: storeProject, name: "production", revision: 1 }, volumes: storeVolumes };
          },
        })),
      }),
      Effect.provideService(OrganizationRuntime, {
        cancel: () => Effect.void,
        open: () => {
          const fault = openFault?.(++opens) ?? null;
          if (fault === "unreachable") return Effect.succeed({ status: "unreachable" as const, error: null });
          return fault === null ? Effect.succeed({ status: "connected" as const, connected: session }) : Effect.fail(fault);
        },
      }),
    ))) as typeof runInngestEffect;

  const request = (input: VolumeRunInput) =>
    runEffect(requestVolumeRun({ userId, organizationId }, { volumeId, environment, ...input }));
  const requested = async (input: VolumeRunInput) => {
    const result = await request(input);
    if (!result.ok) throw new Error(result.refusal.message);
    return result.run;
  };
  const waitingOnLock = () => vi.waitFor(async () => {
    const { rows: waiting } = await harness.pool.query(
      "select 1 from pg_stat_activity where datname = current_database() and wait_event_type = 'Lock'");
    if (waiting.length === 0) throw new Error("nothing waited on the held row lock");
  }, { timeout: 5_000, interval: 25 });
  const settled = ["05-cut", "07-start", "03-start", "04-resume", "05-resume", "03-restore", "03-demote", "03-reset-forget-mirror", "03-reset-forget-writer", "04-forget"];
  const sleeps = [
    ...["03-r0", "03-r1", "03-r2", "06-final"].flatMap((prefix) => Array.from({ length: 9 }, (_, poll) => `${prefix}-backoff-${poll}`)),
    ...[...settled, ...["03-r0", "03-r1", "03-r2"].flatMap((round) => [`${round}-snapshot`, `${round}-pull`]), "06-final-pull",
      ...["fsn-2", "fsn-3"].flatMap((server) => [`04-forget-${server}`, `03-forget-${server}`])]
      .flatMap((id) => Array.from({ length: 6 }, (_, check) => `${id}-wait-${check + 1}`)),
    ...Array.from({ length: 8 }, (_, check) => `01-wait-${check}`),
  ].map((id) => ({ id, handler: () => undefined }));
  /**
   * Only a resize reaches these. The engine rebuilds every mock on each of a run's executions, so giving all 1071 to
   * every run made the six-hour mirror test take seconds on them alone.
   */
  const sizeSleeps = ["fsn-1", "fsn-2", "fsn-3"].flatMap((server) => [0, 1, 2].flatMap((index) =>
    Array.from({ length: 119 }, (_, check) => ({ id: `03-${index}-size-${idOf(server)}-wait-${check + 1}`, handler: () => undefined }))));
  const execute = async (runRowId: string, runId = "run-1", steps?: Array<{ id: string; handler: () => object | undefined }>, volume = volumeId, took?: string, attempt = 0) => {
    const { rows: [row] } = await harness.pool.query<{ kind: string }>("select kind from volume_run where id = $1", [runRowId]);
    return new InngestTestEngine({
      function: createRunVolume(new Inngest({ id: "test" }), runEffect),
      events: [{ name: "volume/run.requested", data: { organizationId, environment: "env-1", volumeId: volume, runId: runRowId, took } }],
      transformCtx: (ctx) => ({ ...mockCtx(ctx), runId, attempt }),
      steps: [...(steps ?? []), ...sleeps, ...(row?.kind === "resize" ? sizeSleeps : [])],
    }).execute();
  };
  /** Runs the row as Inngest would from the event its request sent, `took` and all. */
  const deliver = (runRowId: string, runId = "run-1") => {
    const event = send.mock.calls.flatMap(([sent]) => [sent].flat()).find((sent) => sent.id === `volume-run-${runRowId}`);
    if (event === undefined) throw new Error(`no event sent for ${runRowId}`);
    // SAFETY: the event was built by createVolumeRunRequestedEvent.
    const { took } = event.data as { took?: string };
    return execute(runRowId, runId, undefined, volumeId, took);
  };
  const rows = async () => (await harness.pool.query(
    `select id, kind, state, epoch, phase, current, inngest_run_id, message, orphan, volume_id, docker_volume, refquota_bytes, finished_at
     from volume_run order by created_at, id`,
  )).rows as Array<{
    id: string; kind: string; state: string; epoch: string | null; phase: string; current: string | null; inngest_run_id: string | null;
    message: string | null; orphan: boolean; volume_id: string; docker_volume: string; refquota_bytes: string; finished_at: Date | null;
  }>;
  /** The release a failed Move requested, which the test runs as Inngest would. */
  const releaseRow = async () => {
    const found = (await rows()).find((row) => row.kind === "release");
    if (found === undefined) throw new Error("no release row");
    return found;
  };
  const context = (id: string): RunContext => ({
    id, organizationId, environmentId: "env-1", volumeId, volumeName: "data", dockerVolume, kind: "sync", args: { full: false }, orphan: false, refquotaBytes: 0, recordedBytes: 0, gen: 0, phase: "warm", current: null,
  });
  const running = async (id: string) =>
    harness.pool.query(`update volume_run set state = 'running', inngest_run_id = 'run-1' where id = $1`, [id]);

  beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
  afterAll(async () => { await harness?.stop(); });

  beforeEach(async () => {
    verbs = [];
    failNext = new Map();
    beforeSet = new Map();
    beforeStart = null;
    frameFault = null;
    frames = 0;
    openFault = null;
    opens = 0;
    sent = [];
    sizes = [];
    sizing = [];
    held = new Map();
    guids = 0;
    slotIds = 0;
    snapshots = new Map([[base.name, base]]);
    containers = [{ machine: "fsn-1", id: "web-fsn-1", running: true }];
    storeProject = "shop";
    afterNamespaces = null;
    storeVolumes = [{ id: volumeId, name: "data", storage: { kind: "provisioned", maximumBytes: 5_000_000 } }];
    storeEvents = [];
    storeRefuse = null;
    storeInFlight = [];
    storeLatest = null;
    storeProtected = false;
    deployNodes = [{ type: "service", id: "svc-web", name: "web", outcome: "pending", rows: [] }, { type: "volume", id: volumeId, name: "data", outcome: "pending", rows: [] }];
    machines = [
      { name: "fsn-1", pool: true, copy: writerCopy() },
      { name: "fsn-2", pool: true, copy: null },
    ];
    send.mockReset();
    send.mockResolvedValue({ ids: [] });
    await harness.pool.query(`
      truncate table organization, "user", operation_outbox cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
      insert into "user" (id, email, name) values ('${userId}', 'member@example.com', 'Member');
    `);
  });


  describe("C15", () => {
    it("volume_run_row_lifecycle: requested, sent as volume-run-<id>, claimed, then done", async () => {
      const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });

      expect(run).toMatchObject({ volume_id: volumeId, volume_name: "data", kind: "mirror", state: "requested", orphan: false });
      expect(send).toHaveBeenCalledWith(expect.objectContaining({ id: `volume-run-${run.id}`, name: "volume/run.requested" }));
      expect(await rows()).toMatchObject([{ state: "requested", docker_volume: dockerVolume, refquota_bytes: "5000000", inngest_run_id: null }]);

      const output = await execute(run.id);

      expect(output.error).toBeUndefined();
      expect(await rows()).toMatchObject([{ state: "done", inngest_run_id: "run-1", epoch: "1", message: null, finished_at: expect.any(Date) }]);
    });

    it("a request whose event can't be sent stays requested, with its event left for the sweep", async () => {
      send.mockRejectedValueOnce(new Error("Inngest is down"));
      const run = await requested({ kind: "sync", args: { full: false } });

      expect(run.state).toBe("requested");
      expect((await harness.pool.query("select event_id, sent_at from operation_outbox")).rows)
        .toEqual([{ event_id: `volume-run-${run.id}`, sent_at: null }]);
    });

    it("rerun_from_step_refused: a rerun under a new run id stops at its first effect step", async () => {
      const run = await requested({ kind: "sync", args: { full: false } });
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(base) };
      await execute(run.id, "run-1");
      const claimed = { kind: "claimed", run: context(run.id) };
      verbs = [];

      const rerun = await execute(run.id, "run-rerun", [{ id: "00-claim", handler: () => claimed }]);

      expect(rerun.error).toBeDefined();
      expect(verbs).toEqual([]);
      expect(await rows()).toMatchObject([{ state: "done", inngest_run_id: "run-1" }]);
    });

    it("claim_cas_settles_late_event: a second run of a claimed row does nothing", async () => {
      const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });
      await harness.pool.query(`update volume_run set state = 'running', inngest_run_id = 'run-1' where id = $1`, [run.id]);

      const late = await execute(run.id, "run-late");

      expect(late.result).toEqual({ runId: run.id, settled: "running" });
      expect(verbs).toEqual([]);
      expect(await rows()).toMatchObject([{ state: "running", inngest_run_id: "run-1" }]);
    });

    it("claim_replays_owned_row: a replayed claim of this run's own running row carries on", async () => {
      const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });
      await harness.pool.query(`update volume_run set state = 'running', inngest_run_id = 'run-1' where id = $1`, [run.id]);

      const output = await execute(run.id, "run-1");

      expect(output.error).toBeUndefined();
      expect(verbs[0]).toBe("snapshot@fsn-1(w-1-0)");
      expect(await rows()).toMatchObject([{ state: "done", inngest_run_id: "run-1" }]);
    });

    it("adopts an old primary added back as the mirror before any new write, so placement wants no other run", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(base, null, "1", "old_copy") };
      const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.error).toBeUndefined();
      expect(await rows()).toMatchObject([{ state: "done" }]);
      expect(named("fsn-2").copy).toMatchObject({ kind: "slot", newest: base, role: "mirror" });
      const members = machines.map((fake): Member =>
        ({ machine: { id: idOf(fake.name), name: fake.name }, address: "fdcc::1", answered: true, view: { copy: fake.copy } }));
      const row = { volumeId, environmentId: "env-1", mirrors: [], staged: null, unreached: false, deploymentId: null };
      expect(appliedMirrors(observePlacement(members), row)).toEqual([idOf("fsn-2")]);
    });

    it("sweep_closes_terminal_running: closes each stale row by how its run ended, and leaves live ones", async () => {
      const old = "now() - interval '20 minutes'";
      const insert = (id: string, state: string, run: string | null, at: string) => harness.pool.query(
        `insert into volume_run (id, organization_id, environment_id, volume_id, volume_name, docker_volume, kind, args, state, inngest_run_id, created_at, updated_at)
         values ($1, $2, 'env-1', $3, 'data', 'd', 'sync', '{"full":false}', $4, $5, ${at}, ${at})`,
        [id, organizationId, `v-${id.slice(-2)}`, state, run]);
      const id = (suffix: string) => `00000000-0000-4000-8000-0000000000${suffix}`;
      await insert(id("01"), "requested", null, old);
      await insert(id("02"), "requested", null, "now() - interval '5 minutes'");
      await insert(id("03"), "running", "r-completed", old);
      await insert(id("04"), "running", "r-failed", old);
      await insert(id("05"), "running", "r-cancelled", old);
      await insert(id("06"), "running", "r-missing", old);
      await insert(id("07"), "running", "r-running", old);
      await insert(id("08"), "running", "r-completed-recent", "now() - interval '1 minute'");
      const looked: string[] = [];
      const lookup = (runId: string) => Effect.sync(() => {
        looked.push(runId);
        return runId.replace("r-", "").replace("-recent", "") as InngestRunStatus;
      });

      const swept = await runEffect(closeStaleVolumeRuns(lookup));

      expect(swept).toEqual({ not_started: 1, unheld: 0, closed: 4 });
      expect(looked.sort()).toEqual(["r-cancelled", "r-completed", "r-failed", "r-missing", "r-running"]);
      expect(Object.fromEntries((await rows()).map((row) => [row.id.slice(-2), row.state]))).toEqual({
        "01": "not_started", "02": "requested", "03": "lost", "04": "failed", "05": "cancelled", "06": "lost", "07": "running", "08": "running",
      });
    });

    it("closes only a running row of the run that ended, from a failure, a cancellation or a finish", async () => {
      const run = await requested({ kind: "sync", args: { full: false } });
      await harness.pool.query(`update volume_run set state = 'running', inngest_run_id = 'run-1' where id = $1`, [run.id]);

      expect(await runEffect(closeVolumeRun("run-other", "cancellation"))).toBe(0);
      const finished = await new InngestTestEngine({
        function: createCloseFinishedVolumeRun(new Inngest({ id: "test" }), runEffect),
        events: [{ name: "inngest/function.finished", data: { function_id: "ployz-cloud-run-volume", run_id: "run-1", error: { name: "Error", message: "fsn-2 exploded" } } }],
      }).execute();

      expect(finished.result).toEqual({ closed: 1 });
      expect(await rows()).toMatchObject([{ state: "failed", message: "fsn-2 exploded" }]);
      expect(await runEffect(closeVolumeRun("run-1", "cancellation"))).toBe(0);
    });

    it("the finish closer skips another function's runs", async () => {
      const finished = await new InngestTestEngine({
        function: createCloseFinishedVolumeRun(new Inngest({ id: "test" }), runEffect),
        events: [{ name: "inngest/function.finished", data: { function_id: "ployz-cloud-drain-server", run_id: "run-1" } }],
      }).execute();

      expect(finished.result).toEqual({ skipped: true });
    });
  });

  describe("C1", () => {
    it("volume_run_second_request_refused: the refusal names the Volume, how to follow its run and how to stop it, until it ends", async () => {
      const first = await requested({ kind: "mirror", args: { to: "fsn-2" } });

      const second = await request({ kind: "sync", args: { full: false } });

      expect(second).toEqual({
        ok: false,
        refusal: {
          code: "conflict",
          message: `data is busy with a mirror; ployz volume runs data ${first.id} follows it, and ployz volume release data stops it`,
          details: { run: expect.objectContaining({ id: first.id }) },
        },
        busy: expect.objectContaining({ id: first.id, kind: "mirror", state: "requested" }),
      });
      expect(volumeBusyMessage({ id: "r-1", kind: "sync", volumeName: "data" })).toBe("data is busy with a mirror refresh; ployz volume runs data r-1 follows it, and ployz volume release data stops it");
      expect(volumeBusyMessage({ id: "r-1", kind: "release", volumeName: "data" })).toBe("data is busy with a release; ployz volume runs data r-1 follows it");
      expect(await rows()).toHaveLength(1);

      await execute(first.id);
      expect((await request({ kind: "sync", args: { full: false } })).ok).toBe(true);
    });

    it("a move onto the Server holding the writer is settled, so nothing offers it again", async () => {
      expect(await runEffect(planVolumeRun(organizationId, { volumeId, environment, kind: "move", args: { to: "fsn-1" } }))).toEqual({
        ok: false,
        refusal: { code: "conflict", message: "data's writer is already on fsn-1", details: { settled: true } },
      });
    });

    it("a Volume renamed during a run is refused by its new name, so the hints run as printed", async () => {
      const first = await requested({ kind: "mirror", args: { to: "fsn-2" } });
      storeVolumes = storeVolumes.map((volume) => volume.id === volumeId ? { ...volume, name: "files" } : volume);

      expect(await request({ kind: "sync", args: { full: false } })).toMatchObject({
        ok: false,
        refusal: { message: `files is busy with a mirror; ployz volume runs files ${first.id} follows it, and ployz volume release files stops it` },
      });
    });

    it("a Volume renamed after its request resolved is refused by the name it has at the refusal, and the printed runs command finds the run", async () => {
      const first = await requested({ kind: "mirror", args: { to: "fsn-2" } });
      const resolved = await runEffect(resolveVolume(organizationId, environment, volumeId));
      storeVolumes = storeVolumes.map((volume) => volume.id === volumeId ? { ...volume, name: "files" } : volume);

      const refused = await runEffect(insertRun({ userId, organizationId }, { volumeId, kind: "sync", args: { full: false } }, resolved, crypto.randomUUID()));
      expect(refused).toMatchObject({
        ok: false,
        refusal: { message: `files is busy with a mirror; ployz volume runs files ${first.id} follows it, and ployz volume release files stops it` },
      });
      const [, name, id] = /ployz volume runs (\S+) (\S+)/.exec(refused.ok ? "" : refused.refusal.message) ?? [];
      const named = storeVolumes.find((volume) => volume.name === name);
      expect(named?.id).toBe(volumeId);
      expect((await runEffect(listVolumeRuns(organizationId, volumeId))).map((run) => run.id)).toContain(id);
    });

    it("a Project renamed between the refusal's Namespace and Volume reads still refuses the Volume as busy, by the name its run saved", async () => {
      const first = await requested({ kind: "mirror", args: { to: "fsn-2" } });
      const resolved = await runEffect(resolveVolume(organizationId, environment, volumeId));
      afterNamespaces = () => { storeProject = "store"; };

      expect(await runEffect(insertRun({ userId, organizationId }, { volumeId, kind: "sync", args: { full: false } }, resolved, crypto.randomUUID()))).toMatchObject({
        ok: false,
        refusal: { code: "conflict", message: `data is busy with a mirror; ployz volume runs data ${first.id} follows it, and ployz volume release data stops it` },
      });
      expect(storeProject).toBe("store");
    });

    it("a mirror removal under way says it finishes on its own, and a release leaves it to, since it cannot retract a destroy a Machine started", async () => {
      const removal = await requested({ kind: "delete_mirror", args: { slot: "fsn-2", confirmed_name: null, role: "any" } });

      const removing = `data is busy with a mirror removal, which finishes on its own; ployz volume runs data ${removal.id} follows it`;
      expect(await request({ kind: "sync", args: { full: false } })).toMatchObject({ ok: false, refusal: { code: "conflict", message: removing } });
      expect(await request({ kind: "release", args: {} })).toMatchObject({ ok: false, refusal: { code: "conflict", message: removing } });
      expect(await rows()).toMatchObject([{ id: removal.id, kind: "delete_mirror", state: "requested" }]);
    });

    it("a request whose insert lost to a run that ended before its read inserts again, on the now free Volume", async () => {
      const first = await requested({ kind: "mirror", args: { to: "fsn-2" } });
      const other = await harness.pool.connect();
      try {
        await other.query("begin");
        await other.query("select id from volume_run where id = $1 for update", [first.id]);
        const second = request({ kind: "sync", args: { full: false } });
        await waitingOnLock();
        await other.query("update volume_run set state = 'done', finished_at = now() where id = $1", [first.id]);
        await other.query("commit");

        expect(await second).toMatchObject({ ok: true, run: { kind: "sync", state: "requested" } });
      } finally {
        other.release();
      }
      expect((await rows()).map((row) => [row.kind, row.state])).toEqual([["mirror", "done"], ["sync", "requested"]]);
    });
  });


  describe("takeover", () => {
    const lanes = async () => (await harness.pool.query(
      "select id, kind, state, phase, message, gen from volume_run order by gen")).rows as Array<{ id: string; kind: string; state: string; phase: string; message: string | null; gen: string }>;
    const move = async (phase: "warm" | "committed" = "warm") => {
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });
      await harness.pool.query("update volume_run set state = 'running', inngest_run_id = 'run-1', phase = $2, current = $3 where id = $1",
        [run.id, phase, phase === "committed" ? idOf("fsn-2") : null]);
      send.mockClear();
      return run;
    };
    const release = () => request({ kind: "release", args: {} });

    it("a release during a warm Move ends the Move cancelled and starts after it, then tells the Move's run", async () => {
      const taken = await move();

      const released = await release();

      expect(released).toMatchObject({ ok: true, run: { kind: "release", state: "requested" } });
      const [old, next] = await lanes();
      expect(old).toMatchObject({ id: taken.id, state: "cancelled", message: "volume release took this run over" });
      expect(next).toMatchObject({ kind: "release", state: "requested", phase: "warm" });
      expect(Number(next?.gen)).toBeGreaterThan(Number(old?.gen));
      expect(send).toHaveBeenCalledWith(expect.objectContaining({ id: `volume-run-taken-${taken.id}`, name: "volume/run.taken", data: { runId: taken.id } }));
    });

    it("two releases racing for one warm Move: exactly one of them ends it", async () => {
      const taken = await move();

      const results = await Promise.all([release(), release()]);

      const all = await lanes();
      expect(all.filter((lane) => lane.id === taken.id)).toMatchObject([{ state: "cancelled" }]);
      expect(all.filter((lane) => lane.state === "requested")).toHaveLength(1);
      expect(send.mock.calls.flatMap(([event]) => [event].flat()).filter((event) => event.id === `volume-run-taken-${taken.id}`)).toHaveLength(1);
      expect(results.some((result) => result.ok)).toBe(true);
    });

    it("a release on a committed Move refuses, says where it finishes, and changes no row", async () => {
      const committed = await move("committed");
      const before = await lanes();

      expect(await release()).toEqual({
        ok: false,
        refusal: {
          code: "conflict", message: "data is starting on fsn-2; it finishes there",
          details: { run: expect.objectContaining({ id: committed.id }), move_to: "fsn-2" },
        },
        busy: expect.objectContaining({ id: committed.id }),
      });
      expect(await request({ kind: "sync", args: { full: false } })).toEqual({
        ok: false,
        refusal: { code: "conflict", message: "data is starting on fsn-2; it finishes there", details: { run: expect.objectContaining({ id: committed.id }) } },
        busy: expect.objectContaining({ id: committed.id }),
      });
      expect(await lanes()).toEqual(before);
      expect(send).not.toHaveBeenCalled();
    });

    it("a release on a restore lane, or on a committed release no move row explains, refuses with no move_to", async () => {
      const restoring = await requested({ kind: "restore", args: { from: "fsn-2" } });

      expect(await release()).toEqual({
        ok: false,
        refusal: { code: "conflict", message: "data is starting on fsn-2; it finishes there", details: { run: expect.objectContaining({ id: restoring.id }) } },
        busy: expect.objectContaining({ id: restoring.id }),
      });

      await harness.pool.query("update volume_run set kind = 'release', args = '{}', phase = 'committed', current = $2 where id = $1", [restoring.id, idOf("fsn-2")]);
      expect(await release()).toEqual({
        ok: false,
        refusal: { code: "conflict", message: "data is starting on its new Server; it finishes there", details: { run: expect.objectContaining({ id: restoring.id }) } },
        busy: expect.objectContaining({ id: restoring.id }),
      });
    });

    it("a release on a running seed refuses, saying the copy finishes on its own, and the seed keeps running", async () => {
      const [seeding] = (await harness.pool.query(
        `insert into volume_run (organization_id, environment_id, volume_id, volume_name, docker_volume, kind, args, state, inngest_run_id)
         values ($1, 'env-1', $2, 'data', $3, 'seed', $4, 'running', 'run-seed') returning id`,
        [organizationId, volumeId, dockerVolume, JSON.stringify({ into: { volumeId: "vol2", dockerVolume: "shop-production_vol-vol2", name: "data-copy" } })])).rows as Array<{ id: string }>;

      expect(await release()).toEqual({
        ok: false,
        refusal: { code: "conflict", message: "data is being copied into data-copy; it finishes on its own", details: { run: expect.objectContaining({ id: seeding?.id }) } },
        busy: expect.objectContaining({ id: seeding?.id, kind: "seed" }),
      });
      expect(await request({ kind: "sync", args: { full: false } })).toMatchObject({
        ok: false, refusal: { message: "data is being copied into data-copy; it finishes on its own" },
      });
      expect(await lanes()).toMatchObject([{ id: seeding?.id, state: "running" }]);
      expect(send).not.toHaveBeenCalled();
    });

    it("a Deploy's lane refuses a release and a staged placement alike, so only the Deploy ends it", async () => {
      const [deployed] = (await harness.pool.query(
        `insert into volume_run (organization_id, environment_id, volume_id, volume_name, docker_volume, kind, args, state, inngest_run_id)
         values ($1, 'env-1', $2, 'data', $3, 'deploy', '{"deployment":"d-1","resume":null}', 'running', 'deploy-1') returning id`,
        [organizationId, volumeId, dockerVolume])).rows as Array<{ id: string }>;

      expect(await release()).toMatchObject({
        ok: false,
        refusal: { code: "conflict", message: "data is deploying; the Deploy ends its run", details: { run: expect.objectContaining({ id: deployed?.id }) } },
        busy: expect.objectContaining({ id: deployed?.id, kind: "deploy" }),
      });
      expect(await runEffect(requestVolumeRun({ userId, organizationId }, { volumeId, environment, kind: "move", args: { to: "fsn-2" } }, { end: "yielded" }))).toMatchObject({
        ok: false,
        refusal: { code: "conflict", message: "data is deploying; the Deploy ends its run" },
        busy: expect.objectContaining({ id: deployed?.id, kind: "deploy" }),
      });
      expect(await lanes()).toMatchObject([{ id: deployed?.id, kind: "deploy", state: "running" }]);
      expect(send).not.toHaveBeenCalled();
    });

    describe("a lane landing between the read and the insert", () => {
      const landings = {
        sync: '{"full":false}',
        release: "{}",
        delete_mirror: '{"slot":"fsn-2","confirmed_name":null}',
      };

      for (const [kind, args] of Object.entries(landings)) {
        it(`${kind}: a yielded request waits for it to commit, then takes it over and goes first`, async () => {
          const landing = await harness.pool.connect();
          try {
            await landing.query("begin");
            const { rows: [landed] } = await landing.query(
              `insert into volume_run (organization_id, environment_id, volume_id, volume_name, docker_volume, kind, args, state)
               values ($1, 'env-1', $2, 'data', $3, $4, $5, 'requested') returning id`,
              [organizationId, volumeId, dockerVolume, kind, args]) as { rows: Array<{ id: string }> };

            const taking = runEffect(requestVolumeRun({ userId, organizationId }, { volumeId, environment, kind: "mirror", args: { to: "fsn-2" } }, { end: "yielded" }));
            await waitingOnLock();
            await landing.query("commit");

            expect(await taking).toMatchObject({ ok: true, run: { kind: "mirror", state: "requested" } });
            expect(await lanes()).toMatchObject([
              { id: landed?.id, kind, state: "yielded", message: "a Deploy took this run over" },
              { kind: "mirror", state: "requested" },
            ]);
            expect(send).toHaveBeenCalledWith(expect.objectContaining({ name: "volume/run.taken", data: { runId: landed?.id } }));
          } finally {
            landing.release();
          }
        });
      }

      it("a plain request still refuses the lane that landed, naming it", async () => {
        const landing = await harness.pool.connect();
        try {
          await landing.query("begin");
          const { rows: [landed] } = await landing.query(
            `insert into volume_run (organization_id, environment_id, volume_id, volume_name, docker_volume, kind, args, state)
             values ($1, 'env-1', $2, 'data', $3, 'sync', '{"full":false}', 'requested') returning id`,
            [organizationId, volumeId, dockerVolume]) as { rows: Array<{ id: string }> };
          const asking = request({ kind: "mirror", args: { to: "fsn-2" } });
          await waitingOnLock();
          await landing.query("commit");

          expect(await asking).toMatchObject({ ok: false, refusal: { code: "conflict" }, busy: expect.objectContaining({ id: landed?.id, kind: "sync" }) });
          expect(await lanes()).toMatchObject([{ id: landed?.id, state: "requested" }]);
        } finally {
          landing.release();
        }
      });
    });

    it("a release with no run open repairs from the copies, taking nothing over", async () => {
      expect(await release()).toMatchObject({ ok: true });

      expect(await lanes()).toMatchObject([{ kind: "release", state: "requested" }]);
      expect(send).not.toHaveBeenCalledWith(expect.objectContaining({ name: "volume/run.taken" }));
    });

    it("a release that took a Move whose cut flipped plans from the copy the Floor returns", async () => {
      const G = newSnap("f-1");
      containers = [{ machine: "fsn-1", id: "web-fsn-1", running: false }];
      named("fsn-1").copy = writerCopy(base, 0, true);
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(G) };
      beforeSet.set("floor@fsn-1", () => { named("fsn-1").copy = slotCopy(G, null, "3"); });
      await move();
      const released = await release();
      if (!released.ok) throw new Error(released.refusal.message);

      const output = await deliver(released.run.id, "run-2");

      expect(output.result).toEqual({ runId: released.run.id, released: "fsn-1" });
      expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2", "forget@fsn-2", "writer@fsn-1(kept)"]);
      expect(named("fsn-1").copy).toMatchObject({ kind: "root", readonly: false, newest: G });
      expect(containers).toEqual([{ machine: "fsn-1", id: "web-fsn-1", running: true }]);
    });

    it("a release that took a warm Move on a Volume with no Service floors and plans on its own, and ends done", async () => {
      containers = [];
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(base) };
      await move();
      const released = await release();
      if (!released.ok) throw new Error(released.refusal.message);

      const output = await deliver(released.run.id, "run-2");

      expect(output.error).toBeUndefined();
      expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2", "writer@fsn-1(none)"]);
      expect((await lanes()).find((lane) => lane.id === released.run.id)).toMatchObject({ state: "done" });
    });

    it("a release skips the floor on an empty Server that predates it, so a fleet mid-upgrade can still release", async () => {
      containers = [];
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(base) };
      machines.push({ name: "fsn-3", pool: true, copy: null, capabilities: [requirements.volumeMove.capability] });
      await move();
      const released = await release();
      if (!released.ok) throw new Error(released.refusal.message);

      const output = await deliver(released.run.id, "run-2");

      expect(output.error).toBeUndefined();
      expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2", "writer@fsn-1(none)"]);
      expect((await lanes()).find((lane) => lane.id === released.run.id)).toMatchObject({ state: "done" });
    });

    it("a release that took a warm Move before B got a copy floors B too, so the Move's late Mirror cannot land there", async () => {
      await move();
      const released = await release();
      if (!released.ok) throw new Error(released.refusal.message);

      const output = await execute(released.run.id, "run-2");

      expect(output.error).toBeUndefined();
      expect(verbs.slice(0, 2)).toEqual(["floor@fsn-1", "floor@fsn-2"]);
      const gen = Number((await lanes()).find((lane) => lane.id === released.run.id)?.gen);
      expect(sent[1]).toMatchObject({ expect: { kind: "none" }, want: { kind: "floor" }, gen });
      expect(named("fsn-2").copy).toBeNull();
    });

    it("a staged run that took a warm Move whose cut flipped floors A and promotes it back to the writer before it plans", async () => {
      const G = newSnap("f-1");
      containers = [{ machine: "fsn-1", id: "web-fsn-1", running: false }];
      named("fsn-1").copy = slotCopy(G, null, "3");
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(G) };
      await move();
      const staged = await runEffect(requestVolumeRun({ userId, organizationId }, { volumeId, environment, kind: "mirror", args: { to: "fsn-2" } }, { end: "yielded" }));
      if (!staged.ok) throw new Error(staged.refusal.message);
      const run = staged.run;

      await deliver(run.id, "run-2");

      expect(verbs.slice(0, 3)).toEqual(["floor@fsn-1", "floor@fsn-2", "writer@fsn-1(kept)"]);
      expect(sent[2]).toMatchObject({
        expect: { kind: "mirror", newest: G.guid },
        want: { kind: "writer", at: G.guid, start: { kind: "kept", container_id: "web-fsn-1" } },
        gen: sent[0]?.gen,
      });
      expect(named("fsn-1").copy).toMatchObject({ kind: "root", readonly: false, newest: G });
      expect(containers).toEqual([{ machine: "fsn-1", id: "web-fsn-1", running: true }]);
      expect(await lanes()).toMatchObject([{ kind: "move", state: "yielded" }, { id: run.id, state: "done" }]);
    });

    it("a staged Mirror that took a warm Move before B got a copy plans from B as the fence saw it, and the Move's late Mirror to B is stale", async () => {
      const moved = newSnap("w-0-1");
      const taken = await move();
      const staged = await runEffect(requestVolumeRun({ userId, organizationId }, { volumeId, environment, kind: "mirror", args: { to: "fsn-2" } }, { end: "yielded" }));
      if (!staged.ok) throw new Error(staged.refusal.message);
      const dead: RunContext = { ...context(taken.id), kind: "move", args: { to: "fsn-2" }, gen: Number((await lanes()).find((lane) => lane.id === taken.id)?.gen) };
      let late: CopyAnswer | undefined;
      beforeSet.set("snapshot@fsn-1", async () => {
        late = await runEffect(askCopy(dead, { id: idOf("fsn-2"), name: "fsn-2" }, { kind: "none" }, { kind: "mirror", from: "fdcc::1", target: moved.name, drain: 3_000 }));
        inspect(named("fsn-2"));
      });

      const output = await deliver(staged.run.id, "run-2");

      expect(late).toMatchObject({ ok: false, reason: "stale" });
      expect(output.error).toBeUndefined();
      expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2", "snapshot@fsn-1(w-1-0)", "pull@fsn-2(w-0-1)", "pull@fsn-2(dep-10)", "pull@fsn-2(dep-10)", "snapshot@fsn-1(w-1-1)"]);
      expect(sent[4]).toMatchObject({ expect: { kind: "none" }, want: { kind: "mirror", target: base.name } });
      expect(named("fsn-2").copy).toMatchObject({ kind: "slot", newest: base, pull: null });
      expect(await lanes()).toMatchObject([{ kind: "move", state: "yielded" }, { id: staged.run.id, state: "done" }]);
    });

    it("a staged Restore that took a warm Move whose cut flipped, then fails its plan, leaves the Service started on A", async () => {
      const G = newSnap("f-1");
      containers = [{ machine: "fsn-1", id: "web-fsn-1", running: false }];
      named("fsn-1").copy = slotCopy(G, null, "3");
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(G) };
      await move();
      const staged = await runEffect(requestVolumeRun({ userId, organizationId }, { volumeId, environment, kind: "restore", args: { from: "fsn-2" } }, { end: "yielded" }));
      if (!staged.ok) throw new Error(staged.refusal.message);

      await deliver(staged.run.id, "run-2");

      expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2", "writer@fsn-1(kept)"]);
      expect(containers).toEqual([{ machine: "fsn-1", id: "web-fsn-1", running: true }]);
      expect(await lanes()).toMatchObject([{ kind: "move", state: "yielded" }, { id: staged.run.id, state: "failed" }]);
    });
  });

  describe("a Server leaving", () => {
    const lanes = async () => (await harness.pool.query(
      "select id, kind, state, phase, message from volume_run order by gen")).rows as Array<{ id: string; kind: string; state: string; phase: string; message: string | null }>;
    const fsn = (name: string) => ({ id: idOf(name), name });
    const warmMove = async () => {
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });
      await harness.pool.query("update volume_run set state = 'running', inngest_run_id = 'run-1' where id = $1", [run.id]);
      return run;
    };
    /** The Cluster once fsn-2 is gone, after the committed Move cut fsn-1 into a slot holding its final snapshot. */
    const cutLeftOnA = () => { machines = [{ name: "fsn-1", pool: true, copy: slotCopy(newSnap("f-1")) }]; };
    /** A release that took over a Move committed to fsn-2. */
    const committedRelease = async () => {
      const move = await warmMove();
      await harness.pool.query("update volume_run set state = 'cancelled', phase = 'committed', current = $2 where id = $1", [move.id, idOf("fsn-2")]);
      const release = await requested({ kind: "release", args: {} });
      await harness.pool.query("update volume_run set phase = 'committed', current = $2 where id = $1", [release.id, idOf("fsn-2")]);
      return release;
    };
    const releaseRow = async () => {
      const [lane] = (await lanes()).filter((row) => row.kind === "release");
      if (lane === undefined) throw new Error("no release requested");
      return lane;
    };

    it("removing the target of a warm Move takes its lane with a release, and the Service stays on A", async () => {
      const move = await warmMove();

      expect(await runEffect(releaseLanesOff(organizationId, fsn("fsn-2"), []))).toHaveLength(1);

      expect(await lanes()).toMatchObject([
        { id: move.id, state: "cancelled", message: "volume release took this run over" },
        { kind: "release", state: "requested" },
      ]);
      machines = machines.filter((fake) => fake.name !== "fsn-2");
      const output = await execute((await releaseRow()).id, "run-2");
      expect(output.result).toMatchObject({ released: "fsn-1" });
      expect(containers).toEqual([{ machine: "fsn-1", id: "web-fsn-1", running: true }]);
    });

    it("a Server neither bound for nor writing a lane's Volume leaves the lane alone, and a second pass takes nothing", async () => {
      await warmMove();

      expect(await runEffect(releaseLanesOff(organizationId, fsn("fsn-3"), []))).toEqual([]);
      expect(await lanes()).toMatchObject([{ kind: "move", state: "running" }]);

      await runEffect(releaseLanesOff(organizationId, fsn("fsn-2"), []));
      expect(await runEffect(releaseLanesOff(organizationId, fsn("fsn-2"), [dockerVolume]))).toEqual([]);
      expect(await lanes()).toMatchObject([{ kind: "move", state: "cancelled" }, { kind: "release", state: "requested" }]);
    });

    it("a run that starts after the read, once the Move it saw ended, is not the one a leaving Server takes", async () => {
      const move = await warmMove();
      const other = await harness.pool.connect();
      try {
        await other.query("begin");
        await other.query("select id from volume_run where id = $1 for update", [move.id]);
        const releasing = runEffect(releaseLanesOff(organizationId, fsn("fsn-2"), []));
        await waitingOnLock();
        await other.query("update volume_run set state = 'done', finished_at = now() where id = $1", [move.id]);
        await other.query(`
          insert into volume_run (id, organization_id, environment_id, volume_id, volume_name, docker_volume, refquota_bytes, kind, args, state, inngest_run_id)
          select gen_random_uuid(), organization_id, environment_id, volume_id, volume_name, docker_volume, refquota_bytes, 'mirror', '{"to":"fsn-3"}', 'running', 'run-9'
          from volume_run where id = $1`, [move.id]);
        await other.query("commit");

        expect(await releasing).toEqual([]);
      } finally {
        other.release();
      }
      expect(await lanes()).toMatchObject([{ id: move.id, state: "done" }, { kind: "mirror", state: "running" }]);
    });

    it("removing the source of a warm Move releases, and the release names B's copy to restore from", async () => {
      const at = newSnap("f-1");
      await warmMove();

      expect(await runEffect(releaseLanesOff(organizationId, fsn("fsn-1"), [dockerVolume]))).toHaveLength(1);
      machines = [{ name: "fsn-2", pool: true, copy: slotCopy(at) }];
      containers = [];
      await execute((await releaseRow()).id, "run-2");

      expect(await releaseRow()).toMatchObject({
        state: "failed", message: "data has no writer and no stopped Service on its copies; ployz volume restore data --from fsn-2",
      });
    });

    it("removing B after the Move committed fails the release forward and names A's copy to restore from", async () => {
      const at = newSnap("f-1");
      const release = await committedRelease();
      machines = [{ name: "fsn-1", pool: true, copy: slotCopy(at) }];

      await execute(release.id, "run-2");

      expect(await releaseRow()).toMatchObject({
        state: "failed", message: "data's writer was on fsn-2 (removed). Restore from fsn-1's copy: ployz volume restore data --from fsn-1",
      });
    });

    it("removing B after the Move committed names a stopped copy with a snapshot, not a writer elsewhere", async () => {
      const at = newSnap("f-1");
      const release = await committedRelease();
      machines = [{ name: "fsn-1", pool: true, copy: writerCopy() }, { name: "fsn-3", pool: true, copy: slotCopy(at) }];

      await execute(release.id, "run-2");

      expect(await releaseRow()).toMatchObject({
        state: "failed", message: "data's writer was on fsn-2 (removed). Restore from fsn-3's copy: ployz volume restore data --from fsn-3",
      });
    });

    it("removing B after the Move committed with no copy holding a snapshot names none", async () => {
      const release = await committedRelease();
      machines = [{ name: "fsn-1", pool: true, copy: slotCopy(null) }];

      await execute(release.id, "run-2");

      expect(await releaseRow()).toMatchObject({
        state: "failed", message: "data's writer was on fsn-2 (removed). No other server's copy is known to hold its newest data.",
      });
    });

    it.each(["release", "move"] as const)("removing B ends a running committed %s at once, so a restore from A's copy starts", async (kind) => {
      cutLeftOnA();
      const move = await warmMove();
      await harness.pool.query("update volume_run set phase = 'committed', current = $2 where id = $1", [move.id, idOf("fsn-2")]);
      let lane = move.id;
      if (kind === "release") {
        await harness.pool.query("update volume_run set state = 'failed', finished_at = now() where id = $1", [move.id]);
        lane = (await requested({ kind: "release", args: {} })).id;
        await harness.pool.query("update volume_run set state = 'running', inngest_run_id = 'run-2', phase = 'committed', current = $2 where id = $1", [lane, idOf("fsn-2")]);
      }
      send.mockClear();

      await runEffect(releaseLanesOff(organizationId, fsn("fsn-2"), []));

      expect((await lanes()).find((row) => row.id === lane)).toMatchObject({
        state: "failed", message: "data's writer was on fsn-2 (removed). Restore from fsn-1's copy: ployz volume restore data --from fsn-1",
      });
      expect(send).toHaveBeenCalledWith(expect.objectContaining({ name: "volume/run.taken", data: { runId: lane } }));
      expect(await request({ kind: "restore", args: { from: "fsn-1" } })).toMatchObject({ ok: true, run: { kind: "restore" } });
    });

    it("a warm Move that commits to B while B's removal takes it still ends, so a restore from A's copy starts", async () => {
      cutLeftOnA();
      const move = await warmMove();
      const committing = await harness.pool.connect();
      try {
        await committing.query("begin");
        await committing.query("select id from volume_run where id = $1 for update", [move.id]);
        const releasing = runEffect(releaseLanesOff(organizationId, fsn("fsn-2"), []));
        await waitingOnLock();
        await committing.query("update volume_run set phase = 'committed', current = $2 where id = $1", [move.id, idOf("fsn-2")]);
        await committing.query("commit");

        expect(await releasing).toEqual([]);
      } finally {
        // Destroying the client rolls back a transaction a failed lock poll left open, so the removal it blocks ends.
        committing.release(true);
      }
      expect(await lanes()).toMatchObject([{
        id: move.id, state: "failed", message: "data's writer was on fsn-2 (removed). Restore from fsn-1's copy: ployz volume restore data --from fsn-1",
      }]);
      expect(await request({ kind: "restore", args: { from: "fsn-1" } })).toMatchObject({ ok: true, run: { kind: "restore" } });
    });

    it("a committed Move that fails into a release while B's removal ends it still frees the lane, so a restore starts", async () => {
      cutLeftOnA();
      const move = await warmMove();
      await harness.pool.query("update volume_run set phase = 'committed', current = $2 where id = $1", [move.id, idOf("fsn-2")]);
      const failing = await harness.pool.connect();
      try {
        await failing.query("begin");
        await failing.query("select id from volume_run where id = $1 for update", [move.id]);
        const releasing = runEffect(releaseLanesOff(organizationId, fsn("fsn-2"), []));
        await waitingOnLock();
        await failing.query("update volume_run set state = 'failed', finished_at = now() where id = $1", [move.id]);
        await failing.query(`
          insert into volume_run (organization_id, environment_id, volume_id, volume_name, docker_volume, refquota_bytes, kind, args, phase, current)
          select organization_id, environment_id, volume_id, volume_name, docker_volume, refquota_bytes, 'release', '{}', phase, current
          from volume_run where id = $1`, [move.id]);
        await failing.query("commit");

        await releasing;
      } finally {
        failing.release(true);
      }
      expect(await lanes()).toMatchObject([
        { id: move.id, state: "failed" },
        { kind: "release", state: "failed", message: "data's writer was on fsn-2 (removed). Restore from fsn-1's copy: ployz volume restore data --from fsn-1" },
      ]);
      expect(await request({ kind: "restore", args: { from: "fsn-1" } })).toMatchObject({ ok: true, run: { kind: "restore" } });
    });

    it("removing B ends a committed Move with no copy known elsewhere, and names none", async () => {
      const move = await warmMove();
      await harness.pool.query("update volume_run set phase = 'committed', current = $2 where id = $1", [move.id, idOf("fsn-2")]);

      await runEffect(releaseLanesOff(organizationId, fsn("fsn-2"), []));

      expect(await lanes()).toMatchObject([{ id: move.id, state: "failed", message: "data's writer was on fsn-2 (removed). No other server's copy is known to hold its newest data." }]);
    });

    it("removing B ends a committed Move without naming a copy that holds no snapshot", async () => {
      const move = await warmMove();
      await harness.pool.query("update volume_run set phase = 'committed', current = $2 where id = $1", [move.id, idOf("fsn-2")]);
      machines = [{ name: "fsn-1", pool: true, copy: slotCopy(null) }];

      await runEffect(releaseLanesOff(organizationId, fsn("fsn-2"), []));

      expect(await lanes()).toMatchObject([{ id: move.id, state: "failed", message: "data's writer was on fsn-2 (removed). No other server's copy is known to hold its newest data." }]);
    });

    it("removing B ends a committed Move naming no copy when the Cluster does not answer", async () => {
      const move = await warmMove();
      await harness.pool.query("update volume_run set phase = 'committed', current = $2 where id = $1", [move.id, idOf("fsn-2")]);
      cutLeftOnA();
      frameFault = () => true;

      await runEffect(releaseLanesOff(organizationId, fsn("fsn-2"), []));

      expect(await lanes()).toMatchObject([{ id: move.id, state: "failed", message: "data's writer was on fsn-2 (removed). No other server's copy is known to hold its newest data." }]);
    });

    it("observing the Cluster's copies fails while Cloud holds no pairing to ask them, and the copies alone read as none", async () => {
      expect(await runEffect(observeOrganization(organizationId).pipe(Effect.flip, Effect.orDie))).toMatchObject({ _tag: "ClusterNotPaired", organizationId });
      expect(await runEffect(observeOrganizationCopies(organizationId).pipe(Effect.orDie))).toEqual([]);
    });

    it("removing a Server takes a warm Move bound for it by id, though the Cluster no longer names it", async () => {
      const run = await requested({ kind: "move", args: { to: "fsn-2", machine: idOf("fsn-2") } });
      await harness.pool.query("update volume_run set state = 'running', inngest_run_id = 'run-1' where id = $1", [run.id]);

      expect(await runEffect(releaseLanesOff(organizationId, { id: idOf("fsn-2"), name: null }, []))).toHaveLength(1);
      expect(await lanes()).toMatchObject([{ id: run.id, state: "cancelled" }, { kind: "release", state: "requested" }]);
    });

    it("removing a Server leaves a committed lane bound elsewhere alone", async () => {
      const move = await warmMove();
      await harness.pool.query("update volume_run set phase = 'committed', current = $2 where id = $1", [move.id, idOf("fsn-2")]);

      await runEffect(releaseLanesOff(organizationId, fsn("fsn-1"), [dockerVolume]));

      expect(await lanes()).toMatchObject([{ id: move.id, state: "running", phase: "committed" }]);
    });
  });

  describe("lane gen", () => {
    const gens = async () => (await harness.pool.query(
      "select gen from volume_run where volume_id = $1 order by created_at, id", [volumeId])).rows.map((row: { gen: string }) => Number(row.gen));

    it("rises with each run on a Volume", async () => {
      const first = await requested({ kind: "sync", args: { full: false } });
      await execute(first.id, "run-1");
      const second = await requested({ kind: "sync", args: { full: false } });
      await execute(second.id, "run-2");
      await requested({ kind: "sync", args: { full: false } });

      const [a = 0, b = 0, c = 0] = await gens();
      expect(b).toBeGreaterThan(a);
      expect(c).toBeGreaterThan(b);
    });

    it("never gives two inserts on one Volume the same number, even while both are open", async () => {
      const lane = await Promise.all([1, 2].map(() => request({ kind: "sync", args: { full: false } })));
      expect(lane.filter((result) => result.ok)).toHaveLength(1);
      expect(await gens()).toHaveLength(1);

      const insert = (client: { query: typeof harness.pool.query }) => client.query(
        `insert into volume_run (organization_id, environment_id, volume_id, volume_name, docker_volume, kind, args, state)
         values ($1, 'env-1', $2, 'data', $3, 'sync', '{"full":false}', 'done') returning gen`,
        [organizationId, volumeId, dockerVolume]);
      const one = await harness.pool.connect();
      const two = await harness.pool.connect();
      try {
        await one.query("begin");
        await two.query("begin");
        await Promise.all([insert(one), insert(two)]);
        await Promise.all([one.query("commit"), two.query("commit")]);
      } finally {
        one.release();
        two.release();
      }
      const all = await gens();
      expect(all).toHaveLength(3);
      expect(new Set(all).size).toBe(3);
    });

    it("cannot be chosen by a writer", async () => {
      await expect(harness.pool.query(
        `insert into volume_run (organization_id, environment_id, volume_id, volume_name, docker_volume, kind, args, gen)
         values ($1, 'env-1', $2, 'data', $3, 'sync', '{"full":false}', 1)`,
        [organizationId, volumeId, dockerVolume])).rejects.toThrow(/gen/);
    });

    it("rides on every SetVolumeCopy a full Move sends", async () => {
      first(machines).writes = 1;
      const move = await requested({ kind: "move", args: { to: "fsn-2" } });

      await execute(move.id);

      const [gen] = await gens();
      expect(verbs.at(-2)).toBe("writer@fsn-2(create)");
      expect(sent.map((payload) => payload.gen)).toEqual(sent.map(() => gen));
    });

    it("is the gen of the row that sent it, so a Move's release sends its own", async () => {
      failNext.set("cut@fsn-1", "written");
      const move = await requested({ kind: "move", args: { to: "fsn-2" } });
      await execute(move.id);
      const fromMove = sent.length;

      await execute((await releaseRow()).id, "run-2");

      const [moveGen = 0, releaseGen = 0] = await gens();
      expect(releaseGen).toBeGreaterThan(moveGen);
      expect(sent.slice(0, fromMove).map((payload) => payload.gen)).toEqual(sent.slice(0, fromMove).map(() => moveGen));
      expect(sent.slice(fromMove).map((payload) => payload.gen)).toEqual([releaseGen, releaseGen, releaseGen]);
    });

    it("starts a new row warm", async () => {
      await requested({ kind: "mirror", args: { to: "fsn-2" } });

      expect((await harness.pool.query("select phase from volume_run")).rows).toEqual([{ phase: "warm" }]);
    });
  });

  describe("claim by row", () => {
    it("a duplicate event for a finished row exits at the claim and leaves the row as the first run left it", async () => {
      const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });
      await execute(run.id, "run-1");
      const before = await rows();
      verbs = [];

      const duplicate = await execute(run.id, "run-duplicate");

      expect(duplicate.result).toEqual({ runId: run.id, settled: "done" });
      expect(verbs).toEqual([]);
      expect(await rows()).toEqual(before);
    });

    it("two events for one row at once run it once", async () => {
      const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });

      const both = await Promise.all([execute(run.id, "run-1"), execute(run.id, "run-2")]);

      expect(both.map((output) => output.result).filter((result) => expect.objectContaining({ settled: expect.any(String) }).asymmetricMatch(result))).toHaveLength(1);
      const [row] = await rows();
      expect(row).toMatchObject({ state: "done" });
      expect(["run-1", "run-2"]).toContain(row?.inngest_run_id);
    });
  });

  describe("lane sweep", () => {
    const old = "now() - interval '20 minutes'";
    const insert = (volume: string, state: string, run: string | null, at: string) => harness.pool.query(
      `insert into volume_run (organization_id, environment_id, volume_id, volume_name, docker_volume, kind, args, state, inngest_run_id, created_at, updated_at)
       values ($1, 'env-1', $2, 'data', 'd', 'move', '{"to":"fsn-2"}', $3, $4, ${at}, ${at})`,
      [organizationId, volume, state, run]);
    const byVolume = async () => Object.fromEntries((await harness.pool.query(
      "select volume_id, state, message from volume_run")).rows.map((row: { volume_id: string; state: string; message: string | null }) =>
      [row.volume_id, [row.state, row.message]]));

    it("closes an unclaimed requested row after the grace period as never delivered, and leaves a fresh one", async () => {
      await insert("v-old", "requested", null, old);
      await insert("v-fresh", "requested", null, "now() - interval '1 minute'");

      const swept = await runEffect(closeStaleVolumeRuns(() => Effect.succeed("running" as const)));

      expect(swept).toEqual({ not_started: 1, unheld: 0, closed: 0 });
      expect(await byVolume()).toEqual({ "v-old": ["not_started", "event never delivered"], "v-fresh": ["requested", null] });
    });

    const lane = async (volume: string) => (await harness.pool.query(
      "select kind, state, phase, current, epoch from volume_run where volume_id = $1 order by created_at, id", [volume])).rows;

    it("a warm Move whose Inngest run is over ends and starts the release back to A, and a live one is left", async () => {
      await insert("v-over", "running", "r-over", old);
      await insert("v-live", "running", "r-live", old);

      const swept = await runEffect(closeStaleVolumeRuns((runId) => Effect.succeed(runId === "r-over" ? "cancelled" as const : "running" as const)));

      expect(swept).toEqual({ not_started: 0, unheld: 0, closed: 1 });
      expect(await lane("v-over")).toEqual([
        { kind: "move", state: "cancelled", phase: "warm", current: null, epoch: null },
        { kind: "release", state: "requested", phase: "warm", current: null, epoch: null },
      ]);
      expect(await lane("v-live")).toEqual([{ kind: "move", state: "running", phase: "warm", current: null, epoch: null }]);
      expect(send).toHaveBeenCalledWith(expect.objectContaining({ name: "volume/run.requested", data: expect.objectContaining({ volumeId: "v-over" }) }));
    });

    it("a committed Move whose Inngest run is over starts the release forward to B, at the Move's epoch", async () => {
      await insert("v-over", "running", "r-over", old);
      await harness.pool.query("update volume_run set phase = 'committed', current = $1, epoch = 3", [idOf("fsn-2")]);

      await runEffect(closeStaleVolumeRuns(() => Effect.succeed("cancelled" as const)));

      expect(await lane("v-over")).toEqual([
        { kind: "move", state: "cancelled", phase: "committed", current: idOf("fsn-2"), epoch: "3" },
        { kind: "release", state: "requested", phase: "committed", current: idOf("fsn-2"), epoch: "3" },
      ]);
    });

    it("a Deploy whose Inngest run is over ends and requests the move it took over again, once", async () => {
      const deploy = (volume: string, resume: string) => harness.pool.query(
        `insert into volume_run (organization_id, environment_id, volume_id, volume_name, docker_volume, kind, args, state, inngest_run_id, created_at, updated_at)
         values ($1, 'env-1', $2, 'data', 'd', 'deploy', $3, 'running', 'r-deploy', ${old}, ${old})`,
        [organizationId, volume, `{"deployment":"d-1","resume":${resume}}`]);
      await deploy("v-took", '{"to":"fsn-2"}');
      await deploy("v-plain", "null");

      const swept = await runEffect(closeStaleVolumeRuns(() => Effect.succeed("failed" as const)));

      expect(swept).toEqual({ not_started: 0, unheld: 0, closed: 2 });
      expect(await lane("v-took")).toEqual([
        { kind: "deploy", state: "failed", phase: "warm", current: null, epoch: null },
        { kind: "move", state: "requested", phase: "warm", current: null, epoch: null },
      ]);
      expect(await lane("v-plain")).toEqual([{ kind: "deploy", state: "failed", phase: "warm", current: null, epoch: null }]);
      expect(send).toHaveBeenCalledTimes(1);
      expect(send).toHaveBeenCalledWith(expect.objectContaining({ name: "volume/run.requested", data: expect.objectContaining({ volumeId: "v-took" }) }));
      expect(await runEffect(closeStaleVolumeRuns(() => Effect.succeed("failed" as const)))).toEqual({ not_started: 0, unheld: 0, closed: 0 });
    });
    it("clears the stamps of a Deploy whose Inngest run is over, and keeps a live one's and one it cannot look up", async () => {
      await insert("v-over", "done", "r-mirror", old);
      await insert("v-live", "done", "r-mirror", old);
      await insert("v-unknown", "done", "r-mirror", old);
      await harness.pool.query("update volume_run set held_by = case volume_id when 'v-over' then 'd-over' when 'v-live' then 'd-live' else 'd-unknown' end");

      const swept = await runEffect(closeStaleVolumeRuns((runId) => runId === "d-unknown" ? Effect.fail("unreachable") : Effect.succeed(runId === "d-over" ? "completed" as const : "running" as const)));

      expect(swept).toEqual({ not_started: 0, unheld: 1, closed: 0 });
      expect(Object.fromEntries((await harness.pool.query("select volume_id, held_by from volume_run")).rows.map((row: { volume_id: string; held_by: string | null }) => [row.volume_id, row.held_by])))
        .toEqual({ "v-over": null, "v-live": "d-live", "v-unknown": "d-unknown" });
    });
  });

  describe("stale", () => {
    it("a Stale reply ends the run at once, with no retry and no release", async () => {
      failNext.set("cut@fsn-1", "stale");
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.error).toBeDefined();
      expect(verbs.filter((verb) => verb.startsWith("cut@"))).toEqual(["cut@fsn-1(f-1)"]);
      expect(verbs.at(-1)).toBe("cut@fsn-1(f-1)");
      expect(await rows()).toEqual([expect.objectContaining({ kind: "move", state: "failed", message: "a newer run took data on fsn-1" })]);
      expect(send).toHaveBeenCalledTimes(1);
    });

    it("a Stale reply to a run whose row was taken over leaves the row as the taker left it", async () => {
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });
      beforeSet.set("cut@fsn-1", async () => {
        await harness.pool.query("update volume_run set inngest_run_id = 'run-taker' where id = $1", [run.id]);
        failNext.set("cut@fsn-1", "stale");
      });

      const output = await execute(run.id);

      expect(output.error).toBeDefined();
      expect(verbs.at(-1)).toBe("cut@fsn-1(f-1)");
      expect(await rows()).toEqual([expect.objectContaining({ kind: "move", state: "running", inngest_run_id: "run-taker", message: "Copying to fsn-2", finished_at: null })]);
    });

    it("a run that loses the commit stops like a Stale run, and never tells B to start", async () => {
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });
      const takeOverOnPull = (pulls: number): void => {
        beforeSet.set("pull@fsn-2", async () => {
          if (pulls > 1) return takeOverOnPull(pulls - 1);
          await harness.pool.query("update volume_run set inngest_run_id = 'run-taker' where id = $1", [run.id]);
        });
      };
      takeOverOnPull(4);

      const output = await execute(run.id);

      expect(output.error).toBeDefined();
      expect(verbs.at(-1)).toBe("pull@fsn-2(f-1)");
      expect(verbs.some((verb) => verb.startsWith("writer@"))).toBe(false);
      expect(await rows()).toEqual([expect.objectContaining({ kind: "move", state: "running", phase: "warm", inngest_run_id: "run-taker" })]);
    });
  });

  describe("commit", () => {
    const B = idOf("fsn-2");
    const owned = async () => {
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });
      await running(run.id);
      return { ...context(run.id), kind: "move" as const, args: { to: "fsn-2" } };
    };

    it("wins once, and again on a replay for the run that won it", async () => {
      const ctx = await owned();

      expect(await runEffect(commitMove(ctx, "run-1", B))).toBe(true);
      expect(await runEffect(commitMove(ctx, "run-1", B))).toBe(true);
      expect(await rows()).toMatchObject([{ phase: "committed", current: B, state: "running" }]);
    });

    it("refuses a row already committed to another Machine", async () => {
      const ctx = await owned();
      await runEffect(commitMove(ctx, "run-1", B));

      expect(await runEffect(commitMove(ctx, "run-1", idOf("fsn-3")))).toBe(false);
      expect(await rows()).toMatchObject([{ phase: "committed", current: B }]);
    });

    it("refuses a run whose row was taken over, and sends B nothing", async () => {
      const ctx = await owned();
      await harness.pool.query("update volume_run set inngest_run_id = 'run-taker' where id = $1", [ctx.id]);

      expect(await runEffect(commitMove(ctx, "run-1", B))).toBe(false);
      const answer = await runEffect(commitAndStart(ctx, "run-1", { to: { id: B, name: "fsn-2" }, from: null, expect: { kind: "none" }, at: base.guid, epoch: 1 }));

      expect(answer).toMatchObject({ ok: false, reason: "stale" });
      expect(verbs).toEqual([]);
      expect(await rows()).toMatchObject([{ phase: "warm", current: null, inngest_run_id: "run-taker" }]);
    });

    it("refuses a run whose row was taken over after the commit, and sends B nothing", async () => {
      const ctx = await owned();
      containers = [{ machine: "fsn-1", id: "web-fsn-1", running: false }];
      beforeStart = async () => { await harness.pool.query("update volume_run set inngest_run_id = 'run-taker' where id = $1", [ctx.id]); };

      await expect(runEffect(commitAndStart(ctx, "run-1", { to: { id: B, name: "fsn-2" }, from: null, expect: { kind: "none" }, at: base.guid, epoch: 1 })))
        .rejects.toThrow(/no longer owns/);
      expect(verbs).toEqual([]);
      expect(await rows()).toMatchObject([{ phase: "committed", current: B, inngest_run_id: "run-taker" }]);
    });

    it("starts B from A's stopped Container, not a newer one another Machine keeps", async () => {
      const ctx = await owned();
      machines.push({ name: "fsn-3", pool: true, copy: null });
      containers = [{ machine: "fsn-1", id: "web-fsn-1", running: false, created: 1 }, { machine: "fsn-3", id: "web-fsn-3", running: false, created: 2 }];
      const A = { id: idOf("fsn-1"), name: "fsn-1" };

      await runEffect(commitAndStart(ctx, "run-1", { to: { id: B, name: "fsn-2" }, from: A, expect: { kind: "none" }, at: base.guid, epoch: 1 }));

      expect(sent.at(-1)?.want).toMatchObject({ kind: "writer", start: { kind: "create", resolved_spec: { container: { environment: { POSTGRES_PASSWORD: "secret-fsn-1" } } } } });
    });

    it.each([
      ["the size a resize recorded since the Deploy", 20 * GB, 20 * GB],
      ["the spec's own size while none is recorded", 0, 5_000_000],
    ])("starts B at %s", async (_, recordedBytes, maximumBytes) => {
      const ctx = { ...(await owned()), recordedBytes };
      containers = [{ machine: "fsn-1", id: "web-fsn-1", running: false }];

      await runEffect(commitAndStart(ctx, "run-1", { to: { id: B, name: "fsn-2" }, from: { id: idOf("fsn-1"), name: "fsn-1" }, expect: { kind: "none" }, at: base.guid, epoch: 1 }));

      expect(sent.at(-1)?.want).toMatchObject({
        kind: "writer",
        start: { kind: "create", resolved_spec: { volumes: [{ source: { kind: "provisioned", name: dockerVolume, maximum_bytes: maximumBytes } }] } },
      });
    });

    it.each([
      { rotation: "in", sent: { in: { run: null, expect: "pending" } } },
      { rotation: { held: "run-user" }, sent: "pending" },
      { rotation: "pending", sent: "pending" },
    ] as const)("starts B at A's gen, routed only when A's copy was: $rotation", async ({ rotation, sent: wanted }) => {
      const ctx = await owned();
      containers = [{ machine: "fsn-1", id: "web-fsn-1", running: false, rotation, gen: 3 }];

      await runEffect(commitAndStart(ctx, "run-1", { to: { id: B, name: "fsn-2" }, from: { id: idOf("fsn-1"), name: "fsn-1" }, expect: { kind: "none" }, at: base.guid, epoch: 1 }));

      expect(sent.at(-1)?.want).toMatchObject({ kind: "writer", start: { kind: "create", gen: 3, rotation: wanted } });
    });

    it("with no source named, refuses to guess between stopped Containers on two Machines", async () => {
      const ctx = await owned();
      machines.push({ name: "fsn-3", pool: true, copy: null });
      containers = [{ machine: "fsn-1", id: "web-fsn-1", running: false, created: 1 }, { machine: "fsn-3", id: "web-fsn-3", running: false, created: 2 }];

      await expect(runEffect(commitAndStart(ctx, "run-1", { to: { id: B, name: "fsn-2" }, from: null, expect: { kind: "none" }, at: base.guid, epoch: 1 })))
        .rejects.toThrow("data has 2 stopped Service containers on fsn-1 and fsn-3; starting it on fsn-2 needs exactly one");
      expect(verbs).toEqual([]);
    });
  });

  describe("on failure", () => {
    const B = idOf("fsn-2");
    const owned = async (phase: "warm" | "committed") => {
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });
      await harness.pool.query(
        "update volume_run set state = 'running', inngest_run_id = 'run-1', phase = $2, current = $3, epoch = 4 where id = $1",
        [run.id, phase, phase === "committed" ? B : null]);
      return run;
    };

    const silentThird = (G: Snapshot): Member[] => [
      { machine: { id: idOf("fsn-1"), name: "fsn-1" }, address: "fdcc::1", answered: true, view: { copy: slotCopy(G) }, floorless: false },
      { machine: { id: B, name: "fsn-2" }, address: "fdcc::2", answered: true, view: { copy: slotCopy(G, null, "2") }, floorless: false },
      { machine: { id: idOf("fsn-3"), name: "fsn-3" }, answered: false },
    ];

    it("on a warm row starts a release back to A", async () => {
      const G = newSnap("f-1");
      named("fsn-1").copy = slotCopy(G);
      for (const container of containers) container.running = false;
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(G, null, "2") };
      await owned("warm");

      expect(await runEffect(closeVolumeRun("run-1", "failure"))).toBe(1);
      expect(await rows()).toMatchObject([{ kind: "move", state: "failed" }, { kind: "release", state: "requested", phase: "warm", current: null, epoch: null }]);
      await execute((await releaseRow()).id, "run-2");

      expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2", "forget@fsn-2", "writer@fsn-1(kept)"]);
      expect(named("fsn-1").copy).toMatchObject({ kind: "root", readonly: false, newest: G });
      expect(containers).toEqual([{ machine: "fsn-1", id: "web-fsn-1", running: true }]);
    });

    it("on a warm row waits for a silent Server, noting it on the row, then releases back to A once it answers", async () => {
      const G = newSnap("f-1");
      named("fsn-1").copy = slotCopy(G);
      for (const container of containers) container.running = false;
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(G, null, "2") };
      machines.push({ name: "fsn-3", pool: true, copy: null });
      await owned("warm");
      await runEffect(closeVolumeRun("run-1", "failure"));
      const id = (await releaseRow()).id;
      const silent = { id: "01-observe", handler: () => silentThird(G) };

      const paused = await execute(id, "run-2", [silent, { id: "01-observe-1", handler: () => { throw new Error("paused"); } }]);

      expect(paused.error).toBeDefined();
      expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2"]);
      expect((await rows()).at(-1)).toMatchObject({ id, state: "running", message: "waiting for fsn-3 to answer (check 1 of 8)" });

      verbs = [];
      const output = await execute(id, "run-2", [silent]);

      expect(output.result).toEqual({ runId: id, released: "fsn-1" });
      expect(output.ctx.step.sleep).toHaveBeenCalledWith("01-wait-0", "5s");
      expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2", "floor@fsn-1", "floor@fsn-2", "floor@fsn-3", "forget@fsn-2", "writer@fsn-1(kept)"]);
      expect(containers).toEqual([{ machine: "fsn-1", id: "web-fsn-1", running: true }]);
      expect((await rows()).at(-1)).toMatchObject({ id, state: "done" });
    });

    it("on a warm row fails naming the Server once it stays silent through every wait, and leaves A's Container stopped", async () => {
      const G = newSnap("f-1");
      named("fsn-1").copy = slotCopy(G);
      for (const container of containers) container.running = false;
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(G, null, "2") };
      machines.push({ name: "fsn-3", pool: true, copy: null });
      await owned("warm");
      await runEffect(closeVolumeRun("run-1", "failure"));
      const id = (await releaseRow()).id;
      const observes = Array.from({ length: 9 }, (_, check) => ({ id: check === 0 ? "01-observe" : `01-observe-${check}`, handler: () => silentThird(G) }));

      const output = await execute(id, "run-2", observes);

      expect(output.result).toMatchObject({ refused: { code: "unanswered" } });
      expect(verbs.filter((verb) => !verb.startsWith("floor@"))).toEqual([]);
      expect(containers).toEqual([{ machine: "fsn-1", id: "web-fsn-1", running: false }]);
      expect((await rows()).at(-1)).toMatchObject({ id, state: "failed", message: "fsn-3 did not answer; data's copies are unknown" });
    });

    it("on a committed row finishes on B at the Move's epoch and removes A's stopped Container", async () => {
      const G = newSnap("f-1");
      named("fsn-1").copy = slotCopy(G);
      for (const container of containers) container.running = false;
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(G, null, "2") };
      await owned("committed");

      expect(await runEffect(closeVolumeRun("run-1", "failure"))).toBe(1);
      expect(await rows()).toMatchObject([{ kind: "move", state: "failed" }, { kind: "release", state: "requested", phase: "committed", current: B, epoch: "4" }]);
      const output = await execute((await releaseRow()).id, "run-2");

      expect(output.result).toMatchObject({ moved: "fsn-2" });
      expect(verbs).toEqual(["writer@fsn-2(create)", "remove@fsn-1(web-fsn-1)"]);
      expect(sent.at(-1)).toMatchObject({ want: { kind: "writer", at: G.guid, epoch: 4, start: { kind: "create", creation_key: `${volumeId}-${G.guid}` } } });
      expect(named("fsn-2").copy).toMatchObject({ kind: "root", readonly: false, epoch: 4, newest: G });
      expect(containers).toEqual([{ machine: "fsn-2", id: `created-${volumeId}-${G.guid}`, running: true }]);
      expect((await releaseRow()).state).toBe("done");
    });

    it("on a committed row whose B already runs the Service only removes A's stopped Container", async () => {
      const G = newSnap("f-1");
      named("fsn-1").copy = slotCopy(G);
      for (const container of containers) container.running = false;
      machines[1] = { name: "fsn-2", pool: true, copy: writerCopy(G, 4) };
      containers.push({ machine: "fsn-2", id: "created-1", running: true });
      await owned("committed");

      await runEffect(closeVolumeRun("run-1", "failure"));
      await execute((await releaseRow()).id, "run-2");

      expect(verbs).toEqual(["remove@fsn-1(web-fsn-1)"]);
      expect(containers).toEqual([{ machine: "fsn-2", id: "created-1", running: true }]);
    });

    it("unhealthy_destination_keeps_source_and_fails_run: a B running the Service unrouted while A's copy is routed is asked again, and its failure fails the run before A's stopped Container goes", async () => {
      const G = newSnap("f-1");
      named("fsn-1").copy = slotCopy(G);
      for (const container of containers) Object.assign(container, { running: false, rotation: "in" });
      machines[1] = { name: "fsn-2", pool: true, copy: writerCopy(G, 4) };
      containers.push({ machine: "fsn-2", id: "created-1", running: true, rotation: "pending" });
      await owned("committed");
      failNext.set("writer@fsn-2", new RawFault({ code: "conflict", message: "the moved Service is not routed: unhealthy", details: null }));

      await runEffect(closeVolumeRun("run-1", "failure"));
      const failed = await execute((await releaseRow()).id, "run-2");

      expect(failed.error).toMatchObject({ message: expect.stringContaining("not routed") });
      expect(verbs).toEqual(["writer@fsn-2(create)"]);
      expect(sent.at(-1)?.want).toMatchObject({ kind: "writer", start: { kind: "create", rotation: { in: { run: null, expect: "pending" } } } });
      expect(containers.map(({ id }) => id)).toEqual(["web-fsn-1", "created-1"]);
    });

    it("on a committed row whose forward release fails before B starts, a later release inherits the commitment, finishes on B and removes A's stopped Container", async () => {
      const G = newSnap("f-1");
      named("fsn-1").copy = slotCopy(G);
      for (const container of containers) container.running = false;
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(G, null, "2") };
      await owned("committed");
      await runEffect(closeVolumeRun("run-1", "failure"));
      failNext.set("writer@fsn-2", "mismatch");
      const failed = await execute((await releaseRow()).id, "run-2");
      expect(failed.error).toBeDefined();
      verbs = [];

      const later = await requested({ kind: "release", args: {} });

      expect((await rows()).at(-1)).toMatchObject({ id: later.id, state: "requested", phase: "committed", current: B, epoch: "4" });
      const output = await execute(later.id, "run-3");

      expect(output.result).toMatchObject({ moved: "fsn-2" });
      expect(verbs).toEqual(["writer@fsn-2(create)", "remove@fsn-1(web-fsn-1)"]);
      expect(named("fsn-2").copy).toMatchObject({ kind: "root", readonly: false, epoch: 4, newest: G });
      expect(containers).toEqual([{ machine: "fsn-2", id: `created-${volumeId}-${G.guid}`, running: true }]);
      expect(await rows()).toMatchObject([
        { kind: "move", state: "failed" },
        { kind: "release", state: "failed", phase: "committed", current: B },
        { id: later.id, kind: "release", state: "done" },
      ]);
    });

    describe("a committed Move whose release failed", () => {
      const A = idOf("fsn-1");
      const volume = { environmentId: "env-1", volumeName: "data", dockerVolume, refquotaBytes: 5_000_000, recordedBytes: 0 };
      const finishes = "data finishes on fsn-2; ployz volume release data finishes it now";
      const changed = "data changed since this release was planned; release it again";
      let G: Snapshot;
      const pending = async () => {
        G = newSnap("f-1");
        named("fsn-1").copy = slotCopy(G);
        for (const container of containers) container.running = false;
        machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(G, null, "2") };
        await owned("committed");
        await runEffect(closeVolumeRun("run-1", "failure"));
        const release = await releaseRow();
        await harness.pool.query("update volume_run set state = 'failed', message = 'it failed', finished_at = now() where id = $1", [release.id]);
        send.mockClear();
        return release;
      };
      const insert = (input: VolumeRunInput) => runEffect(insertRun({ userId, organizationId }, { volumeId, ...input }, volume, crypto.randomUUID()));
      const plan = (input: VolumeRunInput) => runEffect(planVolumeRun(organizationId, { volumeId, environment, ...input }));
      const forwardPlan = (after: string, removeOn = [A]) => ({ plan: { sources: [], forgets: [], forward: { after, to: B, removeOn } } });
      const silentA = (): Member[] => [
        { machine: { id: A, name: "fsn-1" }, answered: false },
        { machine: { id: B, name: "fsn-2" }, address: "fdcc::2", answered: true, view: { copy: slotCopy(G, null, "2") }, floorless: false },
      ];

      it("names a Volume renamed since it resolved by its new name, so the release it prints runs", async () => {
        await pending();
        storeVolumes = storeVolumes.map((entry) => entry.id === volumeId ? { ...entry, name: "files" } : entry);

        expect(await insert({ kind: "sync", args: { full: true } })).toMatchObject({
          ok: false,
          refusal: { message: "files finishes on fsn-2; ployz volume release files finishes it now" },
        });
      });

      it("refuses a move, a mirror, any sync, a seed and a removal of B's mirror, of a slot that resolved to no Machine or of every mirror, returning the release that failed as busy, and admits removing another mirror", async () => {
        const release = await pending();
        const before = (await rows()).length;
        const seed: VolumeRunInput = { kind: "seed", args: { into: { volumeId: "vol2", dockerVolume: "shop-production_vol-vol2", name: "data-copy" }, guid: G.guid, name: G.name } };

        for (const input of [
          { kind: "move", args: { to: "fsn-1" } },
          { kind: "mirror", args: { to: "fsn-3" } },
          { kind: "sync", args: { full: true } },
          { kind: "sync", args: { full: false } },
          { kind: "delete_mirror", args: { slot: "fsn-2", confirmed_name: null, machine: B, role: "any" } },
          { kind: "delete_mirror", args: { slot: "fsn-3", confirmed_name: null, role: "any" } },
          { kind: "delete_mirror", args: { slot: null, confirmed_name: null, role: "any" } },
          seed,
        ] satisfies VolumeRunInput[]) {
          expect(await insert(input)).toEqual({
            ok: false,
            refusal: { code: "conflict", message: finishes, details: { run: expect.objectContaining({ id: release.id }) } },
            busy: expect.objectContaining({ id: release.id, kind: "release", state: "failed" }),
          });
        }
        expect(await rows()).toHaveLength(before);
        expect(send).not.toHaveBeenCalled();
        expect(await plan({ kind: "move", args: { to: "fsn-1" } })).toEqual({
          ok: false, refusal: { code: "conflict", message: finishes, details: { run: expect.objectContaining({ id: release.id }) } },
        });
        expect(await insert({ kind: "delete_mirror", args: { slot: "fsn-3", confirmed_name: null, machine: idOf("fsn-3"), role: "any" } })).toMatchObject({ ok: true });
      });

      it("previews a release as going forward to B, removing A's stopped Container, and pins both", async () => {
        const release = await pending();

        const planned = await plan({ kind: "release", args: {} });

        expect(planned).toMatchObject({
          ok: true,
          asked: {
            verb: "release",
            name: "data",
            preview: { volume: volumeId, direction: "forward", to: { machine: B, name: "fsn-2" }, remove_containers_on: ["fsn-1"] },
            effects: [],
          },
          input: { kind: "release", args: forwardPlan(release.id) },
        });
        expect(await rows()).toHaveLength(2);
      });

      it("refuses a release pinned to another commitment, or to none, while one is pending", async () => {
        const release = await pending();
        const before = (await rows()).length;
        const refused = { ok: false, refusal: { code: "conflict", message: changed }, busy: expect.objectContaining({ id: release.id }) };

        expect(await insert({ kind: "release", args: forwardPlan(crypto.randomUUID()) })).toMatchObject(refused);
        expect(await insert({ kind: "release", args: { plan: { sources: [], forgets: [], forward: { after: release.id, to: A, removeOn: [] } } } })).toMatchObject(refused);
        expect(await insert({ kind: "release", args: { plan: { sources: [], forgets: [] } } })).toMatchObject(refused);
        expect(await rows()).toHaveLength(before);
        expect(send).not.toHaveBeenCalled();
      });

      it("refuses a release pinned to go forward once nothing is pending", async () => {
        expect(await insert({ kind: "release", args: forwardPlan(crypto.randomUUID()) })).toMatchObject({ ok: false, refusal: { message: changed } });
        expect(await rows()).toEqual([]);
      });

      it("fails a pinned release whose Servers changed since its plan, starting nothing and keeping the commitment", async () => {
        const release = await pending();
        const planned = await plan({ kind: "release", args: {} });
        if (!planned.ok) throw new Error(planned.refusal.message);
        machines.push({ name: "fsn-3", pool: true, copy: slotCopy(G, null, "3") });
        const id = (await requested(planned.input)).id;

        const output = await execute(id, "run-3");

        expect(output.error).toBeDefined();
        expect(verbs).toEqual([]);
        expect((await rows()).at(-1)).toMatchObject({ id, state: "failed", phase: "committed", current: B, message: changed });
        expect(await insert({ kind: "move", args: { to: "fsn-1" } })).toMatchObject({ ok: false, busy: expect.objectContaining({ id }) });
        expect(release.id).not.toBe(id);
      });

      it("ends the commitment with a restore that takes an epoch, but not with one refused before it does", async () => {
        await pending();
        const refusedRestore = await requested({ kind: "restore", args: { from: "fsn-2" } });
        await execute(refusedRestore.id, "run-r1");
        expect((await rows()).at(-1)).toMatchObject({ id: refusedRestore.id, state: "failed", epoch: null });

        const owed = await requested({ kind: "release", args: {} });
        expect((await rows()).at(-1)).toMatchObject({ id: owed.id, phase: "committed", current: B, epoch: "4" });
        await harness.pool.query("update volume_run set state = 'cancelled', finished_at = now() where id = $1", [owed.id]);

        named("fsn-1").copy = null;
        failNext.set("writer@fsn-2", "mismatch");
        const restored = await requested({ kind: "restore", args: { from: "fsn-2" } });
        await execute(restored.id, "run-r2");
        expect((await rows()).at(-1)).toMatchObject({ id: restored.id, state: "failed", epoch: expect.any(String) });

        const fresh = await requested({ kind: "release", args: {} });
        expect((await rows()).at(-1)).toMatchObject({ id: fresh.id, phase: "warm", current: null, epoch: null });
        expect(await insert({ kind: "mirror", args: { to: "fsn-3" } })).not.toMatchObject({ refusal: { message: finishes } });
      });

      it("waits for a silent A, noting it on the row, then finishes on B once A answers", async () => {
        await pending();
        const id = (await requested({ kind: "release", args: {} })).id;

        const paused = await execute(id, "run-3", [{ id: "01-observe", handler: () => silentA() }, { id: "01-observe-1", handler: () => { throw new Error("paused"); } }]);

        expect(paused.error).toBeDefined();
        expect(verbs).toEqual([]);
        expect((await rows()).at(-1)).toMatchObject({ id, state: "running", message: "waiting for fsn-1 to answer (check 1 of 8)" });

        const output = await execute(id, "run-3", [{ id: "01-observe", handler: () => silentA() }]);

        expect(output.result).toMatchObject({ moved: "fsn-2" });
        expect(output.ctx.step.sleep).toHaveBeenCalledWith("01-wait-0", "5s");
        expect(verbs).toEqual(["writer@fsn-2(create)", "remove@fsn-1(web-fsn-1)"]);
        expect((await rows()).at(-1)).toMatchObject({ id, state: "done" });
      });

      it("waits when the Servers cannot be reached at all, then finishes on B once they answer", async () => {
        await pending();
        const id = (await requested({ kind: "release", args: {} })).id;
        const unreachable = frames + 1;
        frameFault = (read) => read === unreachable;

        const paused = await execute(id, "run-3", [{ id: "01-observe-1", handler: () => { throw new Error("paused"); } }]);

        expect(paused.error).toBeDefined();
        expect(verbs).toEqual([]);
        expect((await rows()).at(-1)).toMatchObject({ id, state: "running", message: "waiting for the Servers to answer (check 1 of 8)" });

        const output = await execute(id, "run-3");

        expect(output.result).toMatchObject({ moved: "fsn-2" });
        expect(verbs).toEqual(["writer@fsn-2(create)", "remove@fsn-1(web-fsn-1)"]);
        expect((await rows()).at(-1)).toMatchObject({ id, state: "done" });
      });

      it("waits when no Server's connection opens, then finishes on B once one does", async () => {
        await pending();
        const id = (await requested({ kind: "release", args: {} })).id;
        const unreachable = opens + 1;
        openFault = (open) => open === unreachable ? "unreachable" : null;

        const paused = await execute(id, "run-3", [{ id: "01-observe-1", handler: () => { throw new Error("paused"); } }]);

        expect(paused.error).toBeDefined();
        expect((await rows()).at(-1)).toMatchObject({ id, state: "running", message: "waiting for the Servers to answer (check 1 of 8)" });

        const output = await execute(id, "run-3");

        expect(output.result).toMatchObject({ moved: "fsn-2" });
        expect((await rows()).at(-1)).toMatchObject({ id, state: "done" });
      });

      it("fails the observe step, without waiting, when Cloud cannot open the session on its own side", async () => {
        await pending();
        const id = (await requested({ kind: "release", args: {} })).id;
        const broken = opens + 1;
        openFault = (open) => open === broken ? new Error("could not decrypt the pairing") : null;

        const output = await execute(id, "run-3", [{ id: "01-observe-1", handler: () => { throw new Error("waited"); } }]);

        expect(output.error).toMatchObject({ message: "could not decrypt the pairing" });
        expect(verbs).toEqual([]);
        expect((await rows()).at(-1)).toMatchObject({ id, state: "running", message: null });
      });

      it("fails the observe step, without waiting, when the Cluster answers its first frame with an error", async () => {
        await pending();
        const id = (await requested({ kind: "release", args: {} })).id;
        const broken = frames + 1;
        frameFault = (read) => read === broken && rpcFault("internal", "decoding the runtime frame failed");

        const output = await execute(id, "run-3", [{ id: "01-observe-1", handler: () => { throw new Error("waited"); } }]);

        expect((await rows()).at(-1)).toMatchObject({ id, state: "running", message: null });
        expect(output.error).toMatchObject({ message: expect.stringContaining("decoding the runtime frame failed") });
        expect(verbs).toEqual([]);
      });

      it("fails once A stays silent past fifteen minutes of checks, keeping the commitment for the next release", async () => {
        await pending();
        const id = (await requested({ kind: "release", args: {} })).id;
        const observes = Array.from({ length: 9 }, (_, check) => ({ id: check === 0 ? "01-observe" : `01-observe-${check}`, handler: () => silentA() }));

        const output = await execute(id, "run-3", observes);

        expect(output.error).toBeDefined();
        expect(verbs).toEqual([]);
        expect((await rows()).at(-1)).toMatchObject({
          id, state: "failed", phase: "committed", current: B,
          message: "fsn-1 did not answer for 15m; data still finishes on fsn-2; ployz volume release data tries again",
        });
        expect(await requested({ kind: "release", args: {} })).toMatchObject({ state: "requested" });
        expect((await rows()).at(-1)).toMatchObject({ phase: "committed", current: B });
      });
    });

    it("on a row it no longer owns does nothing", async () => {
      const run = await owned("committed");
      await harness.pool.query("update volume_run set inngest_run_id = 'run-taker' where id = $1", [run.id]);

      expect(await runEffect(closeVolumeRun("run-1", "failure"))).toBe(0);
      expect(await rows()).toEqual([expect.objectContaining({ kind: "move", state: "running", inngest_run_id: "run-taker" })]);
      expect(send).toHaveBeenCalledTimes(1);
    });

    it("on a run of another kind ends it with no release", async () => {
      const run = await requested({ kind: "sync", args: { full: false } });
      await running(run.id);

      expect(await runEffect(closeVolumeRun("run-1", "failure"))).toBe(1);
      expect(await rows()).toEqual([expect.objectContaining({ kind: "sync", state: "failed" })]);
    });
  });

  describe("yielded", () => {
    it("reads back from volume runs and operation history", async () => {
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });
      await harness.pool.query(
        "update volume_run set state = 'yielded', message = 'a Deploy took the Volume', finished_at = now() where id = $1", [run.id]);

      expect(await runEffect(listVolumeRuns(organizationId, volumeId)))
        .toMatchObject([{ id: run.id, state: "yielded", message: "a Deploy took the Volume" }]);
      expect(await runEffect(operationsOn(organizationId, { kind: "volume", volume: volumeId }, null, 10)))
        .toMatchObject([{ attempt: run.id, state: "superseded", reason: "a Deploy took the Volume" }]);
    });
  });

  describe("epoch", () => {
    it("takes one above the highest epoch a copy shows, in its root or its newest snapshot's name", async () => {
      machines = [
        { name: "fsn-1", pool: true, copy: writerCopy(snap("10", "w-7-2"), 3), writes: 1 },
        { name: "fsn-2", pool: true, copy: null },
        { name: "fsn-3", pool: false, copy: null },
        { name: "docker-1", pool: false, stateless: true, copy: null, silent: true },
      ];
      const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.error).toBeUndefined();
      expect(await rows()).toMatchObject([{ epoch: "8", state: "done" }]);
      expect(verbs).toContain("snapshot@fsn-1(w-8-0)");
    });

    it("takes one above every epoch this Volume's runs took, so a Machine reset can't reuse one", async () => {
      await harness.pool.query(
        `insert into volume_run (organization_id, environment_id, volume_id, volume_name, docker_volume, kind, args, state, epoch, finished_at)
         values ($1, 'env-1', $2, 'data', $3, 'sync', '{"full":false}', 'done', 12, now())`,
        [organizationId, volumeId, dockerVolume]);
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(base) };
      const run = await requested({ kind: "sync", args: { full: true } });

      await execute(run.id);

      expect((await rows()).find((row) => row.id === run.id)).toMatchObject({ epoch: "13", state: "done" });
    });

    it("a replay reads back the epoch the run took", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(base) };
      const run = await requested({ kind: "sync", args: { full: true } });
      await harness.pool.query(`update volume_run set epoch = 40 where id = $1`, [run.id]);

      await execute(run.id);

      expect(await rows()).toMatchObject([{ epoch: "40", state: "done" }]);
      expect(verbs).toContain("snapshot@fsn-1(w-40-0)");
    });
  });

  describe("mirror and sync", () => {
    it("mirror: pulls the writer's newest, and stops once a round finds nothing new", async () => {
      const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, rounds: 1 });
      expect(verbs).toEqual(["snapshot@fsn-1(w-1-0)", "pull@fsn-2(dep-10)", "pull@fsn-2(dep-10)", "snapshot@fsn-1(w-1-1)"]);
      expect(sent[1]).toEqual({ name: dockerVolume, expect: { kind: "none" }, want: { kind: "mirror", from: expect.any(String), target: "dep-10", drain: 3_000 }, gen: expect.any(Number) });
      expect(machines[1]?.copy).toMatchObject({ kind: "slot", newest: base, pull: null });
    });

    it("mirror: a Volume written faster than it ships stops after three rounds", async () => {
      first(machines).writes = 5;
      const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, rounds: 3 });
      expect(verbs).toEqual([
        "snapshot@fsn-1(w-1-0)", "pull@fsn-2(w-1-0)", "pull@fsn-2(w-1-0)",
        "snapshot@fsn-1(w-1-1)", "pull@fsn-2(w-1-1)", "pull@fsn-2(w-1-1)",
        "snapshot@fsn-1(w-1-2)", "pull@fsn-2(w-1-2)", "pull@fsn-2(w-1-2)",
      ]);
      expect(sent[4]?.expect).toEqual({ kind: "mirror", newest: snapshots.get("w-1-0")?.guid, slot: "1001" });
      expect(await rows()).toMatchObject([{ state: "done" }]);
    });

    it("mirror: a pull that fails on the Machine fails the run with its reason", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: null, pullFault: { target: "dep-10", kind: "fail" } };
      const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });

      await execute(run.id);

      expect(await rows()).toMatchObject([{ state: "failed", message: "data-fsn-2 could not receive dep-10: disk full" }]);
    });

    // The engine replays every earlier step on each of the 84 executions these 78 checks take, so this runs in seconds.
    it("mirror: a pull that never lands gives up after six hours of checks backing off to five minutes", { timeout: 20_000 }, async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: null, pullFault: { target: "dep-10", kind: "stall" } };
      const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });

      const output = await new InngestTestEngine({
        function: createRunVolume(new Inngest({ id: "test" }), runEffect),
        events: [{ name: "volume/run.requested", data: { organizationId, environment: "env-1", volumeId, runId: run.id } }],
        // Sleeps as plain calls, not memoized steps, which halves what each of the 84 executions replays.
        transformCtx: (ctx) => {
          const mocked = mockCtx(ctx);
          return { ...mocked, step: { ...mocked.step, sleep: vi.fn<typeof ctx.step.sleep>().mockResolvedValue(undefined) }, runId: "run-1", attempt: 0 };
        },
      }).execute();

      expect(verbs.filter((verb) => verb === "pull@fsn-2(dep-10)")).toHaveLength(PATIENCE_WAITS.length + 1);
      expect(vi.mocked(output.ctx.step.sleep).mock.calls).toEqual(PATIENCE_WAITS.map((wait, check) => [`03-r0-backoff-${check}`, `${wait}s`]));
      expect(await rows()).toMatchObject([{ state: "failed", message: "data-fsn-2 has not finished dep-10 after 6h" }]);
    });

    it("mirror: a pull the 5 s checks started replays none of its checks into the new schedule, so it lands instead of giving up", async () => {
      const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });
      const pulling = { ok: true, copy: slotCopy(null, { target: "dep-10", failure: null }) };
      const old = Array.from({ length: 100 }, (_, poll) => [
        { id: `03-r0-wait-${poll}`, handler: () => undefined },
        { id: `03-r0-repull-${poll}`, handler: () => pulling },
      ]).flat();

      await execute(run.id, "run-1", old);

      expect(await rows()).toMatchObject([{ state: "done" }]);
    });

    it("mirror: a Server without a Pool gets the mirror", async () => {
      machines[1] = { name: "fsn-2", pool: false, copy: null };
      const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, rounds: 1 });
      expect(verbs).toEqual(["snapshot@fsn-1(w-1-0)", "pull@fsn-2(dep-10)", "pull@fsn-2(dep-10)", "snapshot@fsn-1(w-1-1)"]);
      expect(await rows()).toMatchObject([{ state: "done", message: null }]);
    });

    it("a plain sync row with one primary ends without touching a copy, since the mirror sync runs with no row", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(base) };
      const run = await requested({ kind: "sync", args: { full: false } });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, rounds: 0 });
      expect(verbs).toEqual([]);
      expect(await rows()).toMatchObject([{ state: "done" }]);
    });

    it("sync --full: forgets both copies' shared snapshots, then pulls from nothing", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(base) };
      const run = await requested({ kind: "sync", args: { full: true } });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, rounds: 1 });
      expect(verbs).toEqual(["forget@fsn-2", "forget@fsn-1", "snapshot@fsn-1(w-1-0)", "pull@fsn-2(dep-10)", "pull@fsn-2(dep-10)", "snapshot@fsn-1(w-1-1)"]);
      expect(sent[0]?.expect).toEqual({ kind: "mirror", newest: "10", slot: "1" });
      expect(sent[1]?.expect).toEqual({ kind: "writer", epoch: 0 });
    });

    it("a refusal ends the run with its message", async () => {
      failNext.set("snapshot@fsn-1", "no_capacity");
      const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });

      await execute(run.id);

      expect(await rows()).toMatchObject([{ state: "failed", message: "fsn-1 has no room for data's mirror" }]);
    });

    it("busy asks again with the same request after a wait", async () => {
      failNext.set("snapshot@fsn-1", "busy");
      const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, rounds: 1 });
      expect(verbs.slice(0, 2)).toEqual(["snapshot@fsn-1(w-1-0)", "snapshot@fsn-1(w-1-0)"]);
      expect(sent[0]).toEqual(sent[1]);
    });
  });

  describe("delete_mirror", () => {
    it("forgets the slot, then the writer's snapshots shared with it", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(base) };
      const run = await requested({ kind: "delete_mirror", args: { slot: "fsn-2", confirmed_name: null, role: "any" } });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, destroyed: ["fsn-2"] });
      expect(verbs).toEqual(["forget@fsn-2", "forget@fsn-1"]);
      expect(sent[0]?.expect).toEqual({ kind: "mirror", newest: base.guid, slot: "1" });
      expect(machines[1]?.copy).toBeNull();
      expect(await rows()).toMatchObject([{ state: "done", epoch: null }]);
    });

    it("refuses an old copy a late pull made the mirror after the run looked, and deletes nothing", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(base, null, "7", "old_copy") };
      beforeSet.set("forget@fsn-2", () => { named("fsn-2").copy = slotCopy(base, null, "7", "mirror"); });
      const run = await requested({ kind: "delete_mirror", args: { slot: "fsn-2", confirmed_name: null, machine: idOf("fsn-2"), role: "old_copy" } });

      await execute(run.id);

      expect(sent.at(-1)?.expect).toEqual({ kind: "mirror", newest: base.guid, slot: "7", role: "old_copy" });
      expect(verbs).toEqual(["forget@fsn-2"]);
      expect(machines[1]?.copy).toEqual(slotCopy(base, null, "7", "mirror"));
      expect(await rows()).toMatchObject([{ state: "failed", message: "data-fsn-2 is no longer an old copy, so nothing was deleted" }]);
    });

    const LEGACY = "data's delete was requested before Ployz recorded which copy it deletes, so nothing was deleted. Delete the copy again.";
    const legacyOldCopyDelete = async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(base, null, "7", "old_copy") };
      const admitted = await runEffect(deleteOldCopy(organizationId, userId, { volumeId, environment, server: "fsn-2" }));
      if (!admitted.ok) throw new Error(admitted.refusal.message);
      await harness.pool.query(`update volume_run set args = args - 'role' where id = $1`, [admitted.run.id]);
      named("fsn-2").copy = slotCopy(base, null, "7", "mirror");
      return admitted.run.id;
    };

    it("refuses an old copy's delete admitted before runs recorded the role, and deletes nothing", async () => {
      const id = await legacyOldCopyDelete();

      await execute(id);

      expect(verbs.filter((verb) => verb.startsWith("forget"))).toEqual([]);
      expect(machines[1]?.copy).toEqual(slotCopy(base, null, "7", "mirror"));
      expect(await rows()).toMatchObject([{ state: "failed", message: LEGACY }]);
    });

    it("refuses such a delete resumed while already running, and deletes nothing", async () => {
      const id = await legacyOldCopyDelete();
      await harness.pool.query(`update volume_run set state = 'running', inngest_run_id = 'run-1' where id = $1`, [id]);

      await execute(id, "run-1", undefined, volumeId, undefined, 1);

      expect(verbs.filter((verb) => verb.startsWith("forget"))).toEqual([]);
      expect(machines[1]?.copy).toEqual(slotCopy(base, null, "7", "mirror"));
      expect(await rows()).toMatchObject([{ state: "failed", message: LEGACY }]);
    });

    it("deletes only the old copy it was admitted for, though a pull made it the mirror before the run looked", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(base, null, "7", "old_copy") };
      const admitted = await runEffect(deleteOldCopy(organizationId, userId, { volumeId, environment, server: "fsn-2" }));
      if (!admitted.ok) throw new Error(admitted.refusal.message);
      named("fsn-2").copy = slotCopy(base, null, "7", "mirror");

      await execute(admitted.run.id);

      expect(verbs.filter((verb) => verb.startsWith("forget"))).toEqual([]);
      expect(machines[1]?.copy).toEqual(slotCopy(base, null, "7", "mirror"));
      expect(await rows()).toMatchObject([{ state: "failed", message: "data-fsn-2 is no longer an old copy, so nothing was deleted" }]);
    });
  });

  describe("move", () => {
    const at = (name: string) => {
      const found = snapshots.get(name);
      if (found === undefined) throw new Error(`no snapshot ${name}`);
      return found;
    };
    /** A Move cut on fsn-1 at `final`: fsn-1 is a slot keeping its stopped Container. */
    const cut = (final: Snapshot) => {
      named("fsn-1").copy = slotCopy(final);
      for (const container of containers) container.running = false;
    };

    it("rounds, the image, the cut, the final pull, B's start, then A's Container goes", async () => {
      first(machines).writes = 1;
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, moved: "fsn-2" });
      expect(verbs).toEqual([
        "snapshot@fsn-1(w-1-0)", "pull@fsn-2(w-1-0)", "pull@fsn-2(w-1-0)", "snapshot@fsn-1(w-1-1)",
        "copy_image@fsn-1->fsn-2(web-fsn-1)",
        "cut@fsn-1(f-1)", "pull@fsn-2(f-1)", "pull@fsn-2(f-1)",
        "writer@fsn-2(create)", "remove@fsn-1(web-fsn-1)",
      ]);
      const G = at("f-1");
      expect(sent.at(-1)).toMatchObject({
        expect: { kind: "mirror", newest: G.guid },
        want: {
          kind: "writer",
          at: G.guid,
          epoch: 1,
          start: {
            kind: "create",
            namespace: "shop-production",
            creation_key: `${volumeId}-${G.guid}`,
            resolved_spec: { container: { pull_policy: "never", environment: { POSTGRES_PASSWORD: "secret-fsn-1" } } },
          },
        },
      });
      expect(named("fsn-2").copy).toMatchObject({ kind: "root", readonly: false, epoch: 1, newest: G });
      expect(containers).toEqual([{ machine: "fsn-2", id: `created-${volumeId}-${G.guid}`, running: true }]);
      expect(await rows()).toMatchObject([{ state: "done", epoch: "1" }]);
    });

    it("says each step on the row as it reaches it, then how long the Service was stopped", async () => {
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });
      const seen: Array<string | null> = [];
      const message = async () => (await rows())[0]?.message ?? null;
      for (const key of ["snapshot@fsn-1", "cut@fsn-1", "writer@fsn-2"]) beforeSet.set(key, async () => { seen.push(await message()); });

      await execute(run.id);

      expect(seen).toEqual(["Copying to fsn-2", "Copying to fsn-2", "Starting web on fsn-2"]);
      expect(await rows()).toMatchObject([{ state: "done", message: expect.stringMatching(/^web was stopped \d+s$/) }]);
    });

    it("says the Service stopped only once a cut answered busy stops it, and times the stop from that answer to B's", async () => {
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });
      const seen: Array<string | null> = [];
      const message = async () => (await rows())[0]?.message ?? null;
      let clock = 0;
      const now = vi.spyOn(Date, "now").mockImplementation(() => clock);
      failNext.set("cut@fsn-1", "busy");
      beforeSet.set("cut@fsn-1", async () => {
        seen.push(await message());
        beforeSet.set("cut@fsn-1", async () => {
          seen.push(await message());
          clock = 60_000;
        });
      });
      const finalPull = async () => {
        if (named("fsn-1").copy?.kind === "slot") seen.push(await message());
        else beforeSet.set("pull@fsn-2", finalPull);
      };
      beforeSet.set("pull@fsn-2", finalPull);
      beforeSet.set("writer@fsn-2", () => {
        clock = 86_000;
      });

      try {
        await execute(run.id);
      } finally {
        now.mockRestore();
      }

      expect(verbs.filter((verb) => verb.startsWith("cut@"))).toHaveLength(2);
      expect(seen).toEqual(["Copying to fsn-2", "Copying to fsn-2", "web stopped for the final copy"]);
      expect(await rows()).toMatchObject([{ state: "done", message: "web was stopped 26s" }]);
    });

    it("a cut answered busy is asked again until the Container stopped", async () => {
      failNext.set("cut@fsn-1", "busy");
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, moved: "fsn-2" });
      expect(verbs.filter((verb) => verb.startsWith("cut@"))).toEqual(["cut@fsn-1(f-1)", "cut@fsn-1(f-1)"]);
    });

    it("a cut refused as written fails the Move and starts a release that leaves A writing and running", async () => {
      failNext.set("cut@fsn-1", "written");
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.error).toBeDefined();
      expect(verbs.at(-1)).toBe("cut@fsn-1(f-1)");
      expect(await rows()).toMatchObject([
        { kind: "move", state: "failed", message: "data on fsn-1 was written after its final snapshot" },
        { kind: "release", state: "requested", phase: "warm" },
      ]);
      const release = await releaseRow();
      expect(send).toHaveBeenLastCalledWith(expect.objectContaining({ id: `volume-run-${release.id}` }));

      await execute(release.id, "run-2");

      expect(verbs.at(-1)).toBe("writer@fsn-1(kept)");
      expect(sent.at(-1)).toMatchObject({ expect: { kind: "writer", epoch: 0 }, want: { kind: "writer", epoch: 2 } });
      expect(named("fsn-1").copy).toMatchObject({ kind: "root", readonly: false, epoch: 2 });
      expect(containers).toEqual([{ machine: "fsn-1", id: "web-fsn-1", running: true }]);
      expect((await releaseRow()).state).toBe("done");
    });

    it("a cut whose reply was lost fails only the attempt, and the retry reads the cut as done", async () => {
      failNext.set("cut@fsn-1", "lost");
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });

      const lost = await execute(run.id);

      expect(lost.error).toMatchObject({ name: "Error", message: "set_volume_copy on fsn-1: unavailable: target Machine RPC timed out" });
      expect(await rows()).toMatchObject([{ state: "running" }]);
      expect(named("fsn-1").copy).toMatchObject({ kind: "slot", readonly: true });
      verbs = [];

      const retried = await execute(run.id);

      expect(retried.result).toEqual({ runId: run.id, moved: "fsn-2" });
      expect(verbs).toEqual(["pull@fsn-2(f-1)", "pull@fsn-2(f-1)", "writer@fsn-2(create)", "remove@fsn-1(web-fsn-1)"]);
    });

    it("a cut whose answer was lost says no downtime once its retried attempt finishes the Move", async () => {
      failNext.set("cut@fsn-1", "lost");
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });
      const lost = await execute(run.id);
      expect(lost.error).toBeDefined();
      expect(named("fsn-1").copy).toMatchObject({ kind: "slot" });
      const ran = vi.mocked(lost.ctx.step.run).mock.calls.map(([id]) => String(id)).filter((id) => id !== "05-cut");
      const memoized = await Promise.all(ran.map(async (id) => {
        // SAFETY: each step this run memoized returned JSON.
        const value = (await lost.state[createHash("sha1").update(id).digest("hex")]) as object | undefined;
        return { id, handler: () => value };
      }));
      verbs = [];

      const retried = await execute(run.id, "run-1", memoized, volumeId, undefined, 1);

      expect(retried.result).toEqual({ runId: run.id, moved: "fsn-2" });
      expect(verbs[0]).toBe("pull@fsn-1(f-1)");
      expect(await rows()).toMatchObject([{ state: "done", message: null }]);
    });

    it("a start whose answer was lost says about how long the Service was stopped once its retried attempt answers", async () => {
      let clock = 0;
      const now = vi.spyOn(Date, "now").mockImplementation(() => clock);
      try {
        failNext.set("writer@fsn-2", "lost");
        beforeSet.set("writer@fsn-2", () => {
          clock = 10_000;
        });
        const run = await requested({ kind: "move", args: { to: "fsn-2" } });
        const lost = await execute(run.id);
        expect(lost.error).toBeDefined();
        const ran = vi.mocked(lost.ctx.step.run).mock.calls.map(([id]) => String(id)).filter((id) => id !== "07-start");
        const memoized = await Promise.all(ran.map(async (id) => {
          // SAFETY: each step this run memoized returned JSON.
          const value = (await lost.state[createHash("sha1").update(id).digest("hex")]) as object | undefined;
          return { id, handler: () => value };
        }));
        clock = 43_000;

        const retried = await execute(run.id, "run-1", memoized, volumeId, undefined, 1);

        expect(retried.result).toEqual({ runId: run.id, moved: "fsn-2" });
        expect(await rows()).toMatchObject([{ state: "done", message: "web was stopped about 43s (its start was retried)" }]);
        // An older version of this function, replaying the memoized start after a rollback, reads `at` as exact.
        // SAFETY: the start step returned JSON.
        const start = (await retried.state[createHash("sha1").update("07-start").digest("hex")]) as object;
        expect(start).not.toHaveProperty("at");
      } finally {
        now.mockRestore();
      }
    });

    it("a final pull that fails releases back to A, forgetting nothing B lacks", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: null, pullFault: { target: "f-1", kind: "fail" } };
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });

      const output = await execute(run.id);
      await execute((await releaseRow()).id, "run-2");

      expect(output.error).toBeDefined();
      expect(verbs.slice(-6)).toEqual(["cut@fsn-1(f-1)", "pull@fsn-2(f-1)", "pull@fsn-2(f-1)", "floor@fsn-1", "floor@fsn-2", "writer@fsn-1(kept)"]);
      expect(verbs.filter((verb) => verb === "pull@fsn-2(f-1)")).toHaveLength(2);
      expect(named("fsn-1").copy).toMatchObject({ kind: "root", readonly: false, epoch: 2, newest: at("f-1") });
      expect(machines.filter((fake) => fake.copy?.kind === "root")).toHaveLength(1);
      expect(await rows()).toMatchObject([
        { kind: "move", state: "failed", message: "data-fsn-2 could not receive f-1: disk full" },
        { kind: "release", state: "done" },
      ]);
    });

    it("a final pull a restart of B stopped resumes and lands", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: null, pullFault: { target: "f-1", kind: "restart" } };
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, moved: "fsn-2" });
      expect(verbs.filter((verb) => verb === "pull@fsn-2(f-1)")).toHaveLength(3);
      expect(named("fsn-2")).toMatchObject({ stopped: false, copy: { kind: "root", readonly: false, newest: at("f-1") } });
    });

    const landLatePullBeforeEachAsk = (times: number) => {
      let landed = 0;
      const land = () => {
        landed += 1;
        named("fsn-2").copy = slotCopy(newSnap(`w-0-${landed}`));
        if (landed < times) beforeSet.set("pull@fsn-2", land);
      };
      beforeSet.set("pull@fsn-2", land);
    };

    it("a pull whose mirror moved since the run observed it asks again from the newest B holds, and the Move lands", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(base) };
      landLatePullBeforeEachAsk(1);
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, moved: "fsn-2" });
      expect(sent.filter((payload) => payload.want.kind === "mirror" && payload.expect.kind === "mirror").slice(0, 2).map((payload) => payload.expect)).toEqual([
        { kind: "mirror", newest: base.guid, slot: "1" },
        { kind: "mirror", newest: at("w-0-1").guid, slot: "1" },
      ]);
      expect(named("fsn-2").copy).toMatchObject({ kind: "root", readonly: false, epoch: 1, newest: at("f-1") });
      expect(await rows()).toMatchObject([{ state: "done" }]);
    });

    it("a pull whose mirror keeps moving fails the Move with B's refusal", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(base) };
      landLatePullBeforeEachAsk(10);
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.error).toBeDefined();
      expect(verbs.filter((verb) => verb.startsWith("pull@"))).toHaveLength(4);
      expect(await rows()).toMatchObject([{ kind: "move", state: "failed", message: "data's copy on fsn-2 changed under this run" }, { kind: "release" }]);
    });

    it("a pull refused by a B that became a root fails the Move with B's refusal, asking once", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(base) };
      failNext.set("pull@fsn-2", new RawFault({ code: "failed_precondition", message: "mismatch", details: { reason: "mismatch", copy: writerCopy(base, 5) } }));
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.error).toBeDefined();
      expect(verbs.filter((verb) => verb.startsWith("pull@"))).toEqual(["pull@fsn-2(f-1)"]);
      expect(await rows()).toMatchObject([{ kind: "move", state: "failed", message: "data's copy on fsn-2 changed under this run" }, { kind: "release" }]);
    });

    it("again after the cut continues from the final pull", async () => {
      cut(newSnap("f-1"));
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(base) };
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, moved: "fsn-2" });
      expect(verbs).toEqual(["pull@fsn-2(f-1)", "pull@fsn-2(f-1)", "writer@fsn-2(create)", "remove@fsn-1(web-fsn-1)"]);
      expect(await rows()).toMatchObject([{ epoch: "2" }]);
    });

    it("again once B holds the final snapshot only starts B", async () => {
      const G = newSnap("f-1");
      cut(G);
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(G) };
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, moved: "fsn-2" });
      expect(verbs).toEqual(["writer@fsn-2(create)", "remove@fsn-1(web-fsn-1)"]);
    });

    it("again once B writes and runs the Service only removes A's kept Container", async () => {
      const G = newSnap("f-1");
      cut(G);
      machines[1] = { name: "fsn-2", pool: true, copy: writerCopy(G, 2) };
      containers.push({ machine: "fsn-2", id: "created-1", running: true });
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, moved: "fsn-2" });
      expect(verbs).toEqual(["remove@fsn-1(web-fsn-1)"]);
    });

    it("again once B writes takes the slot keeping the stopped Container as the cut source, not a mirror synced to G", async () => {
      const G = newSnap("f-1");
      machines = [
        { name: "fsn-3", pool: true, copy: slotCopy(G) },
        { name: "fsn-1", pool: true, copy: slotCopy(G) },
        { name: "fsn-2", pool: true, copy: writerCopy(G, 2) },
      ];
      containers = [{ machine: "fsn-1", id: "web-fsn-1", running: false }, { machine: "fsn-2", id: "created-1", running: true }];
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, moved: "fsn-2" });
      expect(verbs).toEqual(["remove@fsn-1(web-fsn-1)"]);
      expect(containers).toEqual([{ machine: "fsn-2", id: "created-1", running: true }]);
    });

    it.each([
      ["a writer at another snapshot", () => writerCopy(base, 5)],
      ["a read-only copy mid-cut", () => writerCopy(snapshots.get("f-1") ?? null, 5, true)],
    ])("a release that finds B %s never makes A the writer", async (_, became) => {
      cut(newSnap("f-1"));
      beforeSet.set("pull@fsn-2", () => { named("fsn-2").copy = became(); });
      failNext.set("pull@fsn-2", "no_capacity");
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });

      const output = await execute(run.id);
      await execute((await releaseRow()).id, "run-2");

      expect(output.error).toBeDefined();
      expect(verbs[0]).toBe("pull@fsn-2(f-1)");
      expect(named("fsn-1").copy).toMatchObject({ kind: "slot" });
      expect(containers).toEqual([{ machine: "fsn-1", id: "web-fsn-1", running: false }]);
      expect(machines.filter((fake) => fake.copy?.kind === "root").map((fake) => fake.name)).toEqual(["fsn-2"]);
    });

    it("refuses a writer with no Service container, before any verb", async () => {
      containers = [];
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.result).toMatchObject({ refused: { code: "invalid", message: "data has no Service container on fsn-1; a move needs exactly one" } });
      expect(verbs).toEqual([]);
      expect(await rows()).toMatchObject([{ state: "failed" }]);
    });
  });

  describe("release", () => {
    const G = () => {
      const made = newSnap("f-1");
      named("fsn-1").copy = slotCopy(made);
      for (const container of containers) container.running = false;
      return made;
    };

    it("a cut stopped on a read-only root makes it the writer again and starts its Container", async () => {
      named("fsn-1").copy = writerCopy(base, 0, true);
      for (const container of containers) container.running = false;
      const run = await requested({ kind: "release", args: {} });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, released: "fsn-1" });
      expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2", "writer@fsn-1(kept)"]);
      expect(named("fsn-1").copy).toMatchObject({ kind: "root", readonly: false });
      expect(containers).toEqual([{ machine: "fsn-1", id: "web-fsn-1", running: true }]);
    });

    it("Release raises the epoch past the cut it ends", async () => {
      named("fsn-1").copy = writerCopy(base, 0, true);
      for (const container of containers) container.running = false;
      const run = await requested({ kind: "release", args: {} });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, released: "fsn-1" });
      expect(sent.at(-1)).toMatchObject({ expect: { kind: "writer", epoch: 0 }, want: { kind: "writer", epoch: 1 } });
      expect(named("fsn-1").copy).toMatchObject({ kind: "root", readonly: false, epoch: 1 });
    });

    it("a cut root with no snapshot refuses Release after floor, with the move to run again", async () => {
      named("fsn-1").copy = writerCopy(null, 0, true);
      for (const container of containers) container.running = false;
      const run = await requested({ kind: "release", args: {} });

      const output = await execute(run.id);

      expect(output.result).toMatchObject({ refused: { code: "no_copy" } });
      expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2"]);
      expect(named("fsn-1").copy).toMatchObject({ kind: "root", readonly: true });
      expect(await rows()).toMatchObject([{ state: "failed", message: "data on fsn-1 stopped before its first snapshot; ployz volume move data --to <another server>" }]);
    });

    it("an idle writer is already where Release leaves it", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(base) };
      const run = await requested({ kind: "release", args: {} });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, released: "fsn-1" });
      expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2", "writer@fsn-1(kept)"]);
      expect(machines[1]?.copy).toEqual(slotCopy(base));
    });

    it("a writer whose Move reached B drops the Container A kept", async () => {
      const at = G();
      machines[1] = { name: "fsn-2", pool: true, copy: writerCopy(at, 1) };
      containers.push({ machine: "fsn-2", id: "created-1", running: true });
      const run = await requested({ kind: "release", args: {} });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, released: "fsn-2" });
      expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2", "writer@fsn-2(kept)", "remove@fsn-1(web-fsn-1)"]);
    });

    it("with only slots, forgets B's copy at G before A becomes the writer again", async () => {
      const at = G();
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(at) };
      const run = await requested({ kind: "release", args: {} });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, released: "fsn-1" });
      expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2", "forget@fsn-2", "writer@fsn-1(kept)"]);
      expect(named("fsn-1").copy).toMatchObject({ kind: "root", readonly: false, epoch: 2, newest: at });
      expect(machines[1]?.copy).toBeNull();
    });

    it("a late Writer to B sent before the undo refuses once the undo is done", async () => {
      const at = G();
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(at) };
      const release = await requested({ kind: "release", args: {} });
      await execute(release.id);
      const move = await requested({ kind: "move", args: { to: "fsn-2" } });
      await running(move.id);
      const { rows: [row] } = await harness.pool.query("select gen from volume_run where id = $1", [move.id]) as { rows: Array<{ gen: string }> };

      const late = await runEffect(setVolumeCopy({ ...context(move.id), kind: "move", args: { to: "fsn-2" }, gen: Number(row?.gen) }, "run-1", { id: idOf("fsn-2"), name: "fsn-2" }, { kind: "mirror", newest: at.guid, slot: "1" }, {
        kind: "writer",
        at: at.guid,
        epoch: 1,
        start: { kind: "kept", container_id: "web-fsn-1" as ContainerId },
      }));

      expect(late).toMatchObject({ ok: false, reason: "mismatch", copy: null });
      expect(machines.filter((fake) => fake.copy?.kind === "root" && !fake.copy.readonly).map((fake) => fake.name)).toEqual(["fsn-1"]);
    });

    it("with only slots, a B that became the writer meanwhile refuses the forget, and A's Container stays stopped", async () => {
      const at = G();
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(at) };
      beforeSet.set("forget@fsn-2", () => { named("fsn-2").copy = writerCopy(at, 1); });
      const run = await requested({ kind: "release", args: {} });

      const output = await execute(run.id);

      expect(output.error).toBeDefined();
      expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2", "forget@fsn-2"]);
      expect(containers).toEqual([{ machine: "fsn-1", id: "web-fsn-1", running: false }]);
      expect(machines.filter((fake) => fake.copy?.kind === "root").map((fake) => fake.name)).toEqual(["fsn-2"]);
      expect(await rows()).toMatchObject([{ state: "failed" }]);
    });

    it("with only slots and no kept Container, says to restore", async () => {
      const at = G();
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(at) };
      containers = [];
      const run = await requested({ kind: "release", args: {} });

      await execute(run.id);

      expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2"]);
      expect(await rows()).toMatchObject([{ state: "failed", message: "data has no writer and no stopped Service on its copies; ployz volume restore data --from fsn-1" }]);
    });
  });

  describe("late requests", () => {
    const writable = () => machines.filter((fake) => fake.copy?.kind === "root" && !fake.copy.readonly).map((fake) => fake.name);
    const land = (key: string) => {
      const request = held.get(key);
      if (request === undefined) throw new Error(`nothing held for ${key}`);
      held.delete(key);
      const refused = request();
      inspect(named("fsn-2"));
      return refused;
    };

    it("a committed Move cancelled while B's start is in flight finishes on B, and the late start changes nothing", async () => {
      failNext.set("writer@fsn-2", "held");
      const move = await requested({ kind: "move", args: { to: "fsn-2" } });
      await execute(move.id, "run-1");
      const G = snapshots.get("f-1");
      expect(sent.at(-1)).toMatchObject({ expect: { kind: "mirror", newest: G?.guid }, want: { kind: "writer", at: G?.guid } });
      await runEffect(closeVolumeRun("run-1", "cancellation"));
      expect(await rows()).toMatchObject([
        { kind: "move", state: "cancelled", phase: "committed", current: idOf("fsn-2") },
        { kind: "release", state: "requested", phase: "committed", current: idOf("fsn-2"), epoch: "1" },
      ]);

      await execute((await releaseRow()).id, "run-2");

      expect(verbs.slice(-2)).toEqual(["writer@fsn-2(create)", "remove@fsn-1(web-fsn-1)"]);
      expect(land("writer@fsn-2")).toBeNull();
      expect(writable()).toEqual(["fsn-2"]);
      expect(containers).toEqual([{ machine: "fsn-2", id: `created-${volumeId}-${G?.guid}`, running: true }]);
    });

    it("a warm Move cancelled mid final pull releases back to A, and the late pull leaves one writable root", async () => {
      beforeSet.set("cut@fsn-1", () => { failNext.set("pull@fsn-2", "held"); });
      const move = await requested({ kind: "move", args: { to: "fsn-2" } });
      await execute(move.id, "run-1");
      expect(sent.at(-1)).toMatchObject({ want: { kind: "mirror", target: "f-1" } });
      await runEffect(closeVolumeRun("run-1", "cancellation"));

      await execute((await releaseRow()).id, "run-2");
      expect(verbs.at(-1)).toBe("writer@fsn-1(kept)");
      land("pull@fsn-2");

      expect(writable()).toEqual(["fsn-1"]);
      expect(containers).toEqual([{ machine: "fsn-1", id: "web-fsn-1", running: true }]);
    });

    it("a cut from an ended run that lands after the release refuses, and A keeps writing and running", async () => {
      failNext.set("cut@fsn-1", "held");
      const move = await requested({ kind: "move", args: { to: "fsn-2" } });
      await execute(move.id, "run-1");
      failNext.set("cut@fsn-1", "written");
      await execute(move.id, "run-1");
      expect(await rows()).toMatchObject([{ kind: "move", state: "failed", epoch: "1" }, { kind: "release", state: "requested" }]);

      await execute((await releaseRow()).id, "run-2");
      const refused = held.get("cut@fsn-1");
      held.delete("cut@fsn-1");

      expect(refused?.()).toBe("mismatch");
      expect(named("fsn-1").copy).toMatchObject({ kind: "root", readonly: false, epoch: 2 });
      expect(containers).toEqual([{ machine: "fsn-1", id: "web-fsn-1", running: true }]);
    });
  });

  describe("restore and demote", () => {
    it("restore makes the only copy the writer and says which writes are lost", async () => {
      machines = [
        { name: "fsn-1", pool: true, copy: null },
        { name: "fsn-2", pool: true, copy: slotCopy(base) },
      ];
      containers = [];
      const run = await requested({ kind: "restore", args: { from: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, restored: "fsn-2" });
      expect(verbs).toEqual(["writer@fsn-2(none)"]);
      expect(named("fsn-2").copy).toMatchObject({ kind: "root", readonly: false, epoch: 1, newest: base });
      expect(await rows()).toMatchObject([{
        state: "done",
        message: `data restored on fsn-2 from ${new Date(at * 1000).toISOString()}; writes after that time are lost`,
      }]);
    });

    it("an old root beside a newer writer is cut read-only and the run asks to be run again", async () => {
      machines = [
        { name: "fsn-1", pool: true, copy: writerCopy(base, 3) },
        { name: "fsn-2", pool: true, copy: writerCopy(snap("20"), 5) },
      ];
      const run = await requested({ kind: "sync", args: { full: false } });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, demoted: "fsn-1" });
      expect(verbs).toEqual(["cut@fsn-1(f-6)"]);
      expect(named("fsn-1").copy).toMatchObject({ kind: "slot", readonly: true });
      expect(await rows()).toMatchObject([{
        state: "failed",
        message: "data-fsn-1 was older than data-fsn-2 and is now read-only; writes to it since fsn-2 took over are lost. Run this again",
      }]);
    });

    it("two writers at one epoch are refused, since neither is old", async () => {
      machines = [
        { name: "fsn-1", pool: true, copy: writerCopy(base, 5) },
        { name: "fsn-2", pool: true, copy: writerCopy(snap("20"), 5) },
      ];
      const run = await requested({ kind: "sync", args: { full: false } });

      const output = await execute(run.id);

      expect(output.result).toMatchObject({ refused: { code: "two_writers" } });
      expect(verbs).toEqual([]);
    });
  });

  describe("requireOwner and SetVolumeCopy", () => {
    const fsn1 = { id: idOf("fsn-1"), name: "fsn-1" } as const;
    const snapshot = (id: string) =>
      runEffect(setVolumeCopy(context(id), "run-1", fsn1, { kind: "writer", epoch: 0 }, { kind: "snapshot", name: "w-1-0" }));

    it("every effect step refuses a row this run no longer owns, before any verb", async () => {
      const run = await requested({ kind: "sync", args: { full: false } });
      await harness.pool.query(`update volume_run set state = 'cancelled', inngest_run_id = 'run-1' where id = $1`, [run.id]);

      const owned = await harness.runEffect(Effect.exit(requireOwner(run.id, "run-1")));
      const asked = await snapshot(run.id).then(() => null, (error: Error) => error);

      expect(Exit.isFailure(owned)).toBe(true);
      expect(asked).toBeInstanceOf(NonRetriableError);
      expect(verbs).toEqual([]);
    });

    it.each([
      ["mismatch", "data's copy on fsn-1 changed under this run"],
      ["no_transition", "data's copy on fsn-1 can't get there from where it is"],
      ["busy", "fsn-1 is busy with data"],
      ["no_capacity", "fsn-1 has no room for data's mirror"],
      ["behind", "data's copy on fsn-1 lacks the snapshot this run needs"],
      ["written", "data on fsn-1 was written after its final snapshot"],
      ["no_source", "data's mirror on fsn-1 has no Server to pull from"],
    ] as const)("%s is an answer with the copy left and its message", async (reason, message) => {
      const run = await requested({ kind: "sync", args: { full: false } });
      await running(run.id);
      failNext.set("snapshot@fsn-1", reason);

      const answer = await snapshot(run.id);

      expect(answer).toEqual({ ok: false, reason, copy: writerCopy(), message });
      expect(await rows()).toMatchObject([{ state: "running", message: null }]);
    });

    it("a pull naming an old slot refuses mismatch and leaves the failed receive for a request on this slot", async () => {
      const run = await requested({ kind: "sync", args: { full: false } });
      await running(run.id);
      const failed = slotCopy(base, { target: "f-1", failure: "disk full" }, "1002");
      machines[1] = { name: "fsn-2", pool: true, copy: failed };
      const fsn2 = { id: idOf("fsn-2"), name: "fsn-2" } as const;
      const pull = (slot: string) => runEffect(setVolumeCopy(context(run.id), "run-1", fsn2,
        { kind: "mirror", slot, newest: base.guid }, { kind: "mirror", from: idOf("fsn-1"), target: "f-1", drain: 3_000 }));

      const stale = await pull("1001");
      const current = await pull("1002");

      expect(stale).toMatchObject({ ok: false, reason: "mismatch", copy: failed });
      expect(current).toEqual({ ok: true, copy: failed });
    });

    it("a failure with no refusal fails only the attempt, keeping the Machine's message", async () => {
      const run = await requested({ kind: "sync", args: { full: false } });
      await running(run.id);
      failNext.set("snapshot@fsn-1", "down");

      const failure = await snapshot(run.id).then(() => null, (error: Error) => error);

      expect(failure).not.toBeInstanceOf(NonRetriableError);
      expect(failure?.message).toBe("set_volume_copy on fsn-1: internal: dataset is busy");
      expect(await rows()).toMatchObject([{ state: "running" }]);
    });

    it("a read SeedVolume's reasons refuse ends the run for good with its message", async () => {
      const run = await requested({ kind: "sync", args: { full: false } });
      await running(run.id);
      failNext.set("inspect@fsn-1", new RawFault({ code: "failed_precondition", message: "volume_switching", details: { reason: "volume_switching" } }));

      const failure = await runEffect(sendSwitch(context(run.id), "run-1", fsn1, { command: "inspect_volume_copy", payload: { name: dockerVolume } }))
        .then(() => null, (error: Error) => error);

      expect(failure).toBeInstanceOf(NonRetriableError);
      expect(await rows()).toMatchObject([{ state: "failed", message: "data on fsn-1 is mid-run; try again once the run ends" }]);
    });
  });

  describe("server lacks", () => {
    it("mirror: a Server whose daemon lacks the volume verbs ends the run saying how to fix it, before any epoch", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: null, lacks: "0.2.4" };
      vi.stubGlobal("fetch", async () => new Response("v0.2.5\n"));
      try {
        const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });

        const output = await execute(run.id);

        expect(output.result).toMatchObject({ refused: { code: "server_lacks" } });
        expect(verbs).toEqual([]);
        expect(await rows()).toMatchObject([{ state: "failed", message: "Volume mirror needs a newer Ployz on fsn-2, which runs 0.2.4. Upgrade it to 0.2.5." }]);
      } finally {
        vi.unstubAllGlobals();
      }
    });

    it("move: a Server already on the channel's release says Volume move is not in it yet, with no upgrade", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(base), lacks: "0.2.4" };
      // The beta channel keeps this pointer out of the process-wide cache the stable-channel cases fill.
      await harness.pool.query("insert into organization_server_upgrades (organization_id, channel) values ($1, 'beta')", [organizationId]);
      vi.stubGlobal("fetch", async () => new Response("v0.2.4\n"));
      try {
        const run = await requested({ kind: "move", args: { to: "fsn-2" } });

        const output = await execute(run.id);

        expect(output.result).toMatchObject({ refused: { code: "server_lacks" } });
        expect(verbs).toEqual([]);
        expect(await rows()).toMatchObject([{ state: "failed", message: "Volume move is not in Ployz 0.2.4 on fsn-2 yet." }]);
      } finally {
        vi.unstubAllGlobals();
      }
    });

    it("delete_mirror: a lacking Server's refusal names Mirror removal", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(base), lacks: "0.2.4" };
      vi.stubGlobal("fetch", async () => new Response("v0.2.5\n"));
      try {
        const run = await requested({ kind: "delete_mirror", args: { slot: "fsn-2", confirmed_name: null, role: "any" } });

        await execute(run.id);

        expect(verbs).toEqual([]);
        expect(await rows()).toMatchObject([{ state: "failed", message: "Mirror removal needs a newer Ployz on fsn-2, which runs 0.2.4. Upgrade it to 0.2.5." }]);
      } finally {
        vi.unstubAllGlobals();
      }
    });

    it("mirror: a lacking Server still refuses as lacking when the Release Channel never answers", { timeout: 30_000 }, async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: null, lacks: "0.2.4" };
      vi.stubGlobal("fetch", () => new Promise<Response>(() => {}));
      try {
        const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });

        const output = await execute(run.id);

        expect(output.result).toMatchObject({ refused: { code: "server_lacks" } });
        expect(await rows()).toMatchObject([{ state: "failed" }]);
        expect((await rows())[0]?.message).toContain("fsn-2");
      } finally {
        vi.unstubAllGlobals();
      }
    });
  });

  const caller: Caller = { userId, organization: { id: organizationId, slug: "acme" }, credential: { kind: "session", id: "session-1" }, role: "owner", support: null, via: "agent" };
  /** A run as the CLI asks for it, planned on the Cluster before any row. */
  const admit = (input: VolumeRunInput) => runEffect(runVolume(caller, { volumeId, environment, ...input }, { key: null, approvalId: null }));

  describe("resize", () => {
    const serves = [requirements.volumeFloor.capability, requirements.volumeSize.capability, requirements.volumeShrink.capability];
    const sized = (name: string, copy: VolumeCopy | null): FakeMachine => ({ name, pool: true, copy, capabilities: serves, bound: 10 * GB });
    const grow = (gigabytes: number): VolumeRunInput => ({ kind: "resize", args: { maximum_bytes: gigabytes * GB } });
    const bounds = () => Object.fromEntries(machines.map((fake) => [fake.name, (fake.bound ?? 0) / GB]));
    const refusal = (details: { reason: string; bound_bytes?: number; capacity?: { code: string; required_growth_bytes?: number }; floor_bytes?: number }) =>
      new RawFault({ code: "failed_precondition", message: details.reason, details });
    const recorded = { event: "volume_resized", environment: "env-1", volume: volumeId, from: 10 * GB, to: 20 * GB };

    beforeEach(() => {
      storeVolumes = [{ id: volumeId, name: "data", storage: { kind: "provisioned", maximumBytes: 10 * GB }, recorded_storage: 10 * GB }];
      machines = [sized("fsn-1", writerCopy()), sized("fsn-2", slotCopy(base)), sized("fsn-3", slotCopy(base, null, "2"))];
    });

    it("grows every mirror, then the primary, then records the size, and says where the Volume is", async () => {
      const run = await requested(grow(20));

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, resized: 20 * GB });
      expect(verbs).toEqual(["size@fsn-2(20)", "size@fsn-3(20)", "size@fsn-1(20)"]);
      expect(sizing).toEqual(["Growing data to 20 GB on fsn-2", "Growing data to 20 GB on fsn-3", "Growing data to 20 GB on fsn-1"]);
      expect(sizes).toStrictEqual(Array.from({ length: 3 }, () => ({ name: dockerVolume, maximum_bytes: 20 * GB })));
      expect(bounds()).toEqual({ "fsn-1": 20, "fsn-2": 20, "fsn-3": 20 });
      expect(storeEvents).toEqual([recorded]);
      expect(await rows()).toMatchObject([{ kind: "resize", state: "done", epoch: null, message: "data is 20 GB on fsn-2, fsn-3, fsn-1" }]);
    });

    describe("of a Volume no Server holds (#3501)", () => {
      const unheld = (latest: number | null) => ({ ...recorded, unheld: { latest_deployment: latest } });

      beforeEach(() => {
        machines = [sized("fsn-1", null), sized("fsn-2", null)];
        storeLatest = 3;
      });

      it("records the size alone, naming the newest Deployment it read before asking the Servers, and asks no Machine", async () => {
        const run = await requested(grow(20));

        const output = await execute(run.id);

        expect(output.result).toEqual({ runId: run.id, resized: 20 * GB });
        expect(verbs).toEqual([]);
        expect(storeEvents).toEqual([unheld(3)]);
        expect(await rows()).toMatchObject([{ state: "done", message: "No Server holds data yet; the next deploy creates it at 20 GB" }]);
      });

      it("a Deploy that created the copy since the run looked refuses the record; looking again, the run grows that copy", async () => {
        // Deployment 4 is admitted after the run read Deployment 3, creates the copy at 10 GB and ends before the record.
        storeRefuse = (event) => {
          if (event.event !== "volume_resized" || !("unheld" in event)) return null;
          storeLatest = 4;
          machines[0] = sized("fsn-1", writerCopy());
          return new RpcError({ code: "conflict", message: "Deployment #4 may create data", details: { reason: "deploying", volume: volumeId, deployment: { id: "dep-4", number: 4 }, unheld: true } });
        };
        const run = await requested(grow(20));

        const output = await execute(run.id);

        expect(output.result).toEqual({ runId: run.id, resized: 20 * GB });
        expect(verbs).toEqual(["size@fsn-1(20)"]);
        expect(bounds()).toMatchObject({ "fsn-1": 20 });
        expect(storeEvents).toEqual([recorded]);
        expect(await rows()).toMatchObject([{ state: "done", message: "data is 20 GB on fsn-1" }]);
      });

      it("looks at the Servers again after reading the Deployments, so an observation an older Cloud memoized first never decides", async () => {
        // An older worker memoized `01-observe` with no copy; Deployment 4 then made one at 10 GB, before `01-deployments` ran.
        machines[0] = sized("fsn-1", writerCopy());
        storeLatest = 4;
        const run = await requested(grow(20));

        const output = await execute(run.id, "run-1", [{ id: "01-observe", handler: () => [] }]);

        expect(output.result).toEqual({ runId: run.id, resized: 20 * GB });
        expect(verbs).toEqual(["size@fsn-1(20)"]);
        expect(storeEvents).toEqual([recorded]);
      });

      it("counts a Server reporting no managed storage as unanswered, and records nothing", async () => {
        machines.push({ name: "docker-1", pool: false, stateless: true, copy: null });

        expect(await admit(grow(20))).toMatchObject({ ok: false, refusal: {
          code: "unavailable", message: "docker-1 reports no managed storage, so whether it holds data is unknown; change its size once docker-1 reports its storage, or after the next deploy creates it",
        } });
        expect(storeEvents).toEqual([]);
        expect(await rows()).toEqual([]);
      });

      it("is refused while a Deployment is in flight, which may be creating the copy now", async () => {
        storeInFlight = [{ id: "dep-4", number: 4 }];

        expect(await admit(grow(20))).toMatchObject({ ok: false, refusal: { code: "conflict", message: "Deployment #4 is in flight; run ployz volume set data --size 20 again when it ends" } });
        expect(storeEvents).toEqual([]);
      });
    });

    it("a copy already bigger holds, and a copy that keeps switching is asked again until it holds", async () => {
      named("fsn-2").bound = 30 * GB;
      named("fsn-3").switching = 2;
      const run = await requested(grow(20));

      await execute(run.id);

      expect(verbs).toEqual(["size@fsn-2(20)", "size@fsn-3(20)", "size@fsn-3(20)", "size@fsn-3(20)", "size@fsn-1(20)"]);
      expect(bounds()).toEqual({ "fsn-1": 20, "fsn-2": 30, "fsn-3": 20 });
      expect(storeEvents).toEqual([recorded]);
      expect(await rows()).toMatchObject([{ state: "done" }]);
    });

    it("asked again for a size every copy holds, asks each again and tells the Store again", async () => {
      const first = await requested(grow(20));
      await execute(first.id);
      verbs.length = 0;
      const again = await requested(grow(20));

      await execute(again.id, "run-2");

      expect(verbs).toEqual(["size@fsn-2(20)", "size@fsn-3(20)", "size@fsn-1(20)"]);
      expect(bounds()).toEqual({ "fsn-1": 20, "fsn-2": 20, "fsn-3": 20 });
      expect(storeEvents).toEqual([recorded, recorded]);
      expect(await rows()).toMatchObject([{ id: first.id, state: "done" }, { id: again.id, state: "done" }]);
    });

    it("a cancellation after the record leaves every copy and the Store at the new size", async () => {
      const run = await requested(grow(20));

      await execute(run.id, "run-1", [{ id: "05-finish", handler: () => undefined }]);
      await runEffect(closeVolumeRun("run-1", "cancellation"));

      expect(bounds()).toEqual({ "fsn-1": 20, "fsn-2": 20, "fsn-3": 20 });
      expect(storeEvents).toEqual([recorded]);
      expect(await rows()).toMatchObject([{ state: "cancelled", message: "the run was cancelled" }]);
    });

    it("the primary without room ends the run with the mirrors grown, the Store at the old size, and what to do", async () => {
      failNext.set("size@fsn-1", refusal({ reason: "no_capacity", capacity: { code: "insufficient_storage", required_growth_bytes: 4 * GB } }));
      const run = await requested(grow(20));

      await execute(run.id);

      expect(verbs).toEqual(["size@fsn-2(20)", "size@fsn-3(20)", "size@fsn-1(20)"]);
      expect(bounds()).toEqual({ "fsn-1": 10, "fsn-2": 20, "fsn-3": 20 });
      expect(storeEvents).toEqual([]);
      expect(await rows()).toMatchObject([{
        state: "failed",
        message: "fsn-1 has no room to grow data to 20 GB. data stays 10 GB. fsn-2, fsn-3 already hold 20 GB. Free space on fsn-1, then run ployz volume set data --size 20 again.",
      }]);
    });

    it("a mirror gone from its Server ends the run before the rest are asked", async () => {
      failNext.set("size@fsn-2", refusal({ reason: "not_found" }));
      const run = await requested(grow(20));

      await execute(run.id);

      expect(verbs).toEqual(["size@fsn-2(20)"]);
      expect(bounds()).toEqual({ "fsn-1": 10, "fsn-2": 10, "fsn-3": 10 });
      expect(await rows()).toMatchObject([{
        state: "failed",
        message: "fsn-2 no longer holds a copy of data. data stays 10 GB. fsn-3, fsn-1 were not asked. Run ployz volume set data --size 20 again.",
      }]);
    });

    it("a copy switching past the run's patience ends it as read-only", { timeout: 60_000 }, async () => {
      named("fsn-2").switching = 200;
      const run = await requested(grow(20));

      await execute(run.id);

      expect(verbs).toHaveLength(120);
      expect(await rows()).toMatchObject([{
        state: "failed",
        message: "fsn-2 kept data read-only for 120 checks. data stays 10 GB. fsn-3, fsn-1 were not asked. Run ployz volume set data --size 20 again.",
      }]);
    });

    it("a Store that recorded another size since the run read it ends the run failed, naming both, and lowers no copy", async () => {
      storeRefuse = () => new RpcError({ code: "conflict", message: "size changed", details: { reason: "size_changed", volume: volumeId, recorded: { kind: "provisioned", maximumBytes: 30 * GB } } });
      const run = await requested(grow(20));

      await execute(run.id);

      expect(bounds()).toEqual({ "fsn-1": 20, "fsn-2": 20, "fsn-3": 20 });
      expect(await rows()).toMatchObject([{
        state: "failed",
        message: "data grew to 20 GB on fsn-2, fsn-3, fsn-1, but the Store records data at 30 GB, not 10 GB; read its size again",
      }]);
    });

    it("a Store that couldn't be asked fails the record step for Inngest to retry, ending nothing", async () => {
      storeRefuse = () => new RpcError({ code: "unavailable", message: "the Store is busy", details: null });
      const run = await requested(grow(20));

      const output = await execute(run.id);

      expect(output.error).toBeDefined();
      expect(bounds()).toEqual({ "fsn-1": 20, "fsn-2": 20, "fsn-3": 20 });
      expect(storeEvents).toEqual([]);
      expect(await rows()).toMatchObject([{ state: "running" }]);
    });

    it("a Volume the Store no longer deploys ends the run failed after every copy grew", async () => {
      storeRefuse = () => new RpcError({ code: "not_found", message: "no such lock", details: null });
      const run = await requested(grow(20));

      await execute(run.id);

      expect(await rows()).toMatchObject([{
        state: "failed",
        message: "data grew to 20 GB on fsn-2, fsn-3, fsn-1, but data is no longer deployed, so its size was not recorded",
      }]);
    });

    it.each([
      ["lists no capabilities", undefined],
      ["lacks the size verb", [requirements.volumeFloor.capability]],
    ])("a holder that %s refuses the run at admission, before any row", async (_, capabilities) => {
      named("fsn-2").capabilities = capabilities;

      expect(await admit(grow(20))).toEqual({ ok: false, refusal: { code: "unsupported", message: "Volume resize is not available on fsn-2.", details: null } });

      expect(verbs).toEqual([]);
      expect(await rows()).toEqual([]);
    });

    it("a holder that lost the size verb since admission fails the run at its first look, asking no Server for a size", async () => {
      const run = await requested(grow(20));
      named("fsn-3").capabilities = [requirements.volumeFloor.capability];

      await execute(run.id);

      expect(verbs).toEqual([]);
      expect(await rows()).toMatchObject([{ state: "failed", message: "Volume resize is not available on fsn-3." }]);
    });

    it("the size the Store already records asks the primary, then each mirror, to hold it, and records nothing", async () => {
      const run = await requested(grow(10));

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, resized: 10 * GB });
      expect(verbs).toEqual(["size@fsn-1(10)", "size@fsn-2(10)", "size@fsn-3(10)"]);
      expect(sizing).toEqual(["Setting data to 10 GB on fsn-1", "Setting data to 10 GB on fsn-2", "Setting data to 10 GB on fsn-3"]);
      expect(sizes).toStrictEqual(Array.from({ length: 3 }, () => ({ name: dockerVolume, maximum_bytes: 10 * GB, allow_shrink: true })));
      expect(bounds()).toEqual({ "fsn-1": 10, "fsn-2": 10, "fsn-3": 10 });
      expect(storeEvents).toEqual([]);
      expect(await rows()).toMatchObject([{ state: "done", message: "data is 10 GB on fsn-1, fsn-2, fsn-3" }]);
    });

    it("is refused while a Move runs, and while a committed Move is owed its release", async () => {
      const move = await requested({ kind: "move", args: { to: "fsn-2" } });
      await running(move.id);

      expect(await request(grow(20))).toMatchObject({ ok: false, refusal: { code: "conflict", message: volumeBusyMessage({ id: move.id, kind: "move", volumeName: "data" }) } });

      await harness.pool.query("update volume_run set state = 'cancelled', phase = 'committed', current = $2, finished_at = now() where id = $1", [move.id, idOf("fsn-2")]);
      expect(await request(grow(20))).toMatchObject({ ok: false, refusal: { code: "conflict", message: "data finishes on fsn-2; ployz volume release data finishes it now" } });
      expect((await rows()).map((row) => row.kind)).toEqual(["move"]);
    });

    describe("to a smaller size", () => {
      const shrink = (gigabytes: number): VolumeRunInput => ({ kind: "resize", args: { maximum_bytes: gigabytes * GB } });
      const shrunk = { event: "volume_resized", environment: "env-1", volume: volumeId, from: 20 * GB, to: 10 * GB };
      const floor = refusal({ reason: "below_floor", bound_bytes: 20 * GB, floor_bytes: 12 * GB });

      beforeEach(() => {
        storeVolumes = [{ id: volumeId, name: "data", storage: { kind: "provisioned", maximumBytes: 20 * GB }, recorded_storage: 20 * GB }];
        for (const fake of machines) fake.bound = 20 * GB;
      });

      it("records the smaller size before any Machine is asked, then shrinks the primary, then each mirror", async () => {
        let askedAtRecord: string[] | null = null;
        storeRefuse = () => {
          askedAtRecord ??= [...verbs];
          return null;
        };
        const run = await requested(shrink(10));

        const output = await execute(run.id);

        expect(output.result).toEqual({ runId: run.id, resized: 10 * GB });
        expect(askedAtRecord).toEqual([]);
        expect(storeEvents).toEqual([shrunk]);
        expect(verbs).toEqual(["size@fsn-1(10)", "size@fsn-2(10)", "size@fsn-3(10)"]);
        expect(sizing).toEqual(["Shrinking data to 10 GB on fsn-1", "Shrinking data to 10 GB on fsn-2", "Shrinking data to 10 GB on fsn-3"]);
        expect(sizes).toStrictEqual(Array.from({ length: 3 }, () => ({ name: dockerVolume, maximum_bytes: 10 * GB, allow_shrink: true })));
        expect(bounds()).toEqual({ "fsn-1": 10, "fsn-2": 10, "fsn-3": 10 });
        expect(await rows()).toMatchObject([{ state: "done", message: "data is 10 GB on fsn-1, fsn-2, fsn-3" }]);
      });

      it("a record the Store refuses while a Deployment is in flight ends the run failed, naming it, and asks no Machine", async () => {
        storeRefuse = (event) => event.event === "volume_resized" && event.to < event.from
          ? new RpcError({ code: "conflict", message: "Deployment #7 is in flight", details: { reason: "deploying", volume: volumeId, deployment: { id: "dep-7", number: 7 } } })
          : null;
        const run = await requested(shrink(10));

        await execute(run.id);

        expect(verbs).toEqual([]);
        expect(storeEvents).toEqual([]);
        expect(bounds()).toEqual({ "fsn-1": 20, "fsn-2": 20, "fsn-3": 20 });
        expect(await rows()).toMatchObject([{
          state: "failed",
          message: "data stays 20 GB on fsn-1, fsn-2, fsn-3, because Deployment #7 is in flight; run ployz volume set data --size 10 again when it ends",
        }]);
      });

      it("is refused at admission while a Deployment is in flight, in the Store's words; a grow and a settle are not", async () => {
        storeInFlight = [{ id: "dep-7", number: 7 }];

        expect(await admit(shrink(10))).toEqual({
          ok: false,
          refusal: { code: "conflict", message: "Deployment #7 is in flight; run ployz volume set data --size 10 again when it ends", details: { reason: "deploying", deployment: { id: "dep-7", number: 7 } } },
        });
        expect(verbs).toEqual([]);
        expect(await rows()).toEqual([]);
        expect(await request(shrink(20))).toMatchObject({ ok: true });
        await harness.pool.query("update volume_run set state = 'cancelled', finished_at = now()");
        expect(await request(shrink(30))).toMatchObject({ ok: true });
      });

      it("the primary below its floor undoes the record and names the floor in whole GB, asking no mirror", async () => {
        failNext.set("size@fsn-1", refusal({ reason: "below_floor", bound_bytes: 20 * GB, floor_bytes: 12_300_000_000 }));
        const run = await requested(shrink(10));

        await execute(run.id);

        expect(verbs).toEqual(["size@fsn-1(10)"]);
        expect(storeEvents).toEqual([shrunk, { ...shrunk, from: 10 * GB, to: 20 * GB }]);
        expect(bounds()).toEqual({ "fsn-1": 20, "fsn-2": 20, "fsn-3": 20 });
        expect(await rows()).toMatchObject([{
          state: "failed",
          message: "fsn-1 can't shrink data to 10 GB; its data needs at least 13 GB. data stays 20 GB. fsn-2, fsn-3 were not asked. Shrink data to at least 13 GB instead, or free space in it on fsn-1, then run ployz volume set data --size 10 again.",
        }]);
      });

      /**
       * Whole-GB rounding leaves up to 1 GB of room for writes between the refusal and the retry; a larger write still
       * refuses, naming the new floor.
       */
      it("the printed floor is accepted when typed back after a small write", async () => {
        named("fsn-1").needs = 3_107_428_864;
        const refused = await requested(shrink(3));

        await execute(refused.id);

        expect(await rows()).toMatchObject([{
          state: "failed",
          message: "fsn-1 can't shrink data to 3 GB; its data needs at least 4 GB. data stays 20 GB. fsn-2, fsn-3 were not asked. Shrink data to at least 4 GB instead, or free space in it on fsn-1, then run ployz volume set data --size 3 again.",
        }]);
        named("fsn-1").needs = 3_107_528_000;
        const retried = await requested(shrink(4));

        await execute(retried.id, "run-2");

        expect(bounds()).toEqual({ "fsn-1": 4, "fsn-2": 4, "fsn-3": 4 });
        expect(await rows()).toMatchObject([{ state: "failed" }, { id: retried.id, state: "done", message: "data is 4 GB on fsn-1, fsn-2, fsn-3" }]);
      });

      it("a mirror below its floor after the primary shrank keeps the record and says the primary holds it", async () => {
        failNext.set("size@fsn-2", floor);
        const run = await requested(shrink(10));

        await execute(run.id);

        expect(verbs).toEqual(["size@fsn-1(10)", "size@fsn-2(10)"]);
        expect(storeEvents).toEqual([shrunk]);
        expect(bounds()).toEqual({ "fsn-1": 10, "fsn-2": 20, "fsn-3": 20 });
        expect(await rows()).toMatchObject([{
          state: "failed",
          message: "fsn-2 can't shrink data to 10 GB; its data needs at least 12 GB. data is 10 GB. fsn-1 already holds 10 GB; fsn-3 was not asked. Free space in data, run ployz volume sync data so fsn-2's copy follows, then run ployz volume set data --size 10 again.",
        }]);
      });

      describe("then the same size again", () => {
        beforeEach(async () => {
          failNext.set("size@fsn-2", floor);
          const run = await requested(shrink(10));
          await execute(run.id);
          storeVolumes = [{ id: volumeId, name: "data", storage: { kind: "provisioned", maximumBytes: 10 * GB }, recorded_storage: 10 * GB }];
          verbs.length = 0;
          sizing.length = 0;
          storeEvents.length = 0;
        });

        it("shrinks the mirrors the refused shrink left larger, and records nothing", async () => {
          const settle = await requested(shrink(10));

          await execute(settle.id, "run-2");

          expect(verbs).toEqual(["size@fsn-1(10)", "size@fsn-2(10)", "size@fsn-3(10)"]);
          expect(sizing).toEqual(["Setting data to 10 GB on fsn-1", "Setting data to 10 GB on fsn-2", "Setting data to 10 GB on fsn-3"]);
          expect(bounds()).toEqual({ "fsn-1": 10, "fsn-2": 10, "fsn-3": 10 });
          expect(storeEvents).toEqual([]);
          expect(await rows()).toMatchObject([{ state: "failed" }, { id: settle.id, state: "done", message: "data is 10 GB on fsn-1, fsn-2, fsn-3" }]);
        });

        it("a mirror still below its floor ends it naming that mirror, and the Store stays at the size", async () => {
          failNext.set("size@fsn-2", floor);
          const settle = await requested(shrink(10));

          await execute(settle.id, "run-2");

          expect(verbs).toEqual(["size@fsn-1(10)", "size@fsn-2(10)"]);
          expect(bounds()).toEqual({ "fsn-1": 10, "fsn-2": 20, "fsn-3": 20 });
          expect(storeEvents).toEqual([]);
          expect(await rows()).toMatchObject([{ state: "failed" }, {
            id: settle.id,
            state: "failed",
            message: "fsn-2 can't shrink data to 10 GB; its data needs at least 12 GB. data is 10 GB. fsn-1 already holds 10 GB; fsn-3 was not asked. Free space in data, run ployz volume sync data so fsn-2's copy follows, then run ployz volume set data --size 10 again.",
          }]);
        });
      });

      it("a primary that keeps failing without a refusal leaves the record in place once the run gives up, undoing nothing", async () => {
        failNext.set("size@fsn-1", "down");
        const run = await requested(shrink(10));

        const output = await execute(run.id);

        expect(output.error).toBeDefined();
        expect(await rows()).toMatchObject([{ state: "running" }]);
        await runEffect(closeVolumeRun("run-1", "failure"));

        expect(verbs).toEqual(["size@fsn-1(10)"]);
        expect(bounds()).toEqual({ "fsn-1": 20, "fsn-2": 20, "fsn-3": 20 });
        expect(storeEvents).toEqual([shrunk]);
        expect(await rows()).toMatchObject([{ state: "failed" }]);
      });

      it("a Store that recorded another size since the run read it ends the run before any Machine is asked", async () => {
        storeRefuse = () => new RpcError({ code: "conflict", message: "size changed", details: { reason: "size_changed", volume: volumeId, recorded: { kind: "provisioned", maximumBytes: 30 * GB } } });
        const run = await requested(shrink(10));

        await execute(run.id);

        expect(verbs).toEqual([]);
        expect(bounds()).toEqual({ "fsn-1": 20, "fsn-2": 20, "fsn-3": 20 });
        expect(await rows()).toMatchObject([{
          state: "failed",
          message: "data stays 20 GB on fsn-1, fsn-2, fsn-3, because the Store records data at 30 GB, not 20 GB; read its size again",
        }]);
      });

      it("a holder without the shrink refuses it at admission, naming the Server, and still admits a grow", async () => {
        named("fsn-2").capabilities = [requirements.volumeFloor.capability, requirements.volumeSize.capability];

        expect(await admit(shrink(10))).toEqual({ ok: false, refusal: { code: "unsupported", message: "Volume shrink is not available on fsn-2.", details: null } });
        expect(verbs).toEqual([]);
        expect(await rows()).toEqual([]);

        expect(await admit(grow(30))).toMatchObject({ ok: true });
      });

      it("a holder that lost the shrink since admission fails the run at its first look, asking no Server for a size", async () => {
        const run = await requested(shrink(10));
        named("fsn-3").capabilities = [requirements.volumeFloor.capability, requirements.volumeSize.capability];

        await execute(run.id);

        expect(verbs).toEqual([]);
        expect(storeEvents).toEqual([]);
        expect(await rows()).toMatchObject([{ state: "failed", message: "Volume shrink is not available on fsn-3." }]);
      });

      it("a stale replay left the Store larger than every copy: a settle refuses, asks no Machine and records nothing", async () => {
        for (const fake of machines) fake.bound = 10 * GB;
        const run = await requested(shrink(20));

        await execute(run.id);

        expect(verbs).toEqual([]);
        expect(sizes).toEqual([]);
        expect(storeEvents).toEqual([]);
        expect(await rows()).toMatchObject([{
          state: "failed",
          message: "fsn-1, fsn-2 and fsn-3 hold data at 10 GB, below its recorded 20 GB. Run ployz volume set data --size 10 to record the size it holds, then ployz volume set data --size 20 to grow it",
        }]);
      });

      it("a Store left larger than every copy by a stale record is set back to the held size, and no copy moves", async () => {
        for (const fake of machines) fake.bound = 10 * GB;
        const run = await requested(shrink(10));

        const output = await execute(run.id);

        expect(output.result).toEqual({ runId: run.id, resized: 10 * GB });
        expect(storeEvents).toEqual([shrunk]);
        expect(verbs).toEqual(["size@fsn-1(10)", "size@fsn-2(10)", "size@fsn-3(10)"]);
        expect(bounds()).toEqual({ "fsn-1": 10, "fsn-2": 10, "fsn-3": 10 });
        expect(await rows()).toMatchObject([{ state: "done", message: "data is 10 GB on fsn-1, fsn-2, fsn-3" }]);
      });
    });

    it("a settle is refused while a Move runs, like any other resize", async () => {
      const move = await requested({ kind: "move", args: { to: "fsn-2" } });
      await running(move.id);

      expect(await request(grow(10))).toMatchObject({ ok: false, refusal: { code: "conflict", message: volumeBusyMessage({ id: move.id, kind: "move", volumeName: "data" }) } });
      expect(verbs).toEqual([]);
      expect((await rows()).map((row) => row.kind)).toEqual(["move"]);
    });

    it("a release takes a warm resize over", async () => {
      const run = await requested(grow(20));
      await running(run.id);

      expect(await request({ kind: "release", args: {} })).toMatchObject({ ok: true, run: { kind: "release", state: "requested" } });

      expect(await rows()).toMatchObject([{ id: run.id, state: "cancelled", message: "volume release took this run over" }, { kind: "release", state: "requested" }]);
    });
  });

  describe("in a Protected Environment", () => {
    const protectorId = "00000000-0000-4000-8000-000000000e03";
    const protector: Caller = { ...caller, userId: protectorId, via: "dashboard" };
    const member: Caller = { ...caller, role: "member", via: "cli" };
    const askers: ReadonlyArray<[string, Caller]> = [
      ["a member", member],
      ["a member's token", { ...member, credential: { kind: "token", id: "token-1", grant: FULL_GRANT } }],
      ["a member's agent", { ...member, via: "agent" }],
    ];
    const as = (who: Caller, input: VolumeRunInput, approvalId: string | null = null) =>
      runEffect(runVolume(who, { volumeId, environment, ...input }, { key: null, approvalId }));
    const askedOf = (admitted: Awaited<ReturnType<typeof as>>) => {
      if (admitted.ok || admitted.refusal.code !== "approval_required") throw new Error(`expected an ask, got ${JSON.stringify(admitted)}`);
      // SAFETY: an approval_required refusal carries the digest and id of the approval it recorded.
      return admitted.refusal.details as { approval: string; approval_id: string };
    };
    const review = async (id: string) =>
      (await harness.pool.query<{ review: unknown }>("select review from operation_approvals where id = $1", [id])).rows[0]?.review;

    beforeEach(async () => {
      storeProtected = true;
      await harness.pool.query(`insert into "user" (id, email, name) values ('${protectorId}', 'owner@example.com', 'Owner')`);
    });

    it.each(askers)("%s's Move asks someone who can protect it, writes no run until they approve, then runs once", async (_, asker) => {
      const move: VolumeRunInput = { kind: "move", args: { to: "fsn-2" } };

      const admitted = await as(asker, move);
      const { approval, approval_id } = askedOf(admitted);

      expect(admitted).toMatchObject({ refusal: { details: { protected: { environment: "production" } } } });
      expect(await rows()).toEqual([]);
      expect(await review(approval_id)).toMatchObject({ protected: { environment: "production" } });
      expect(await runEffect(decideApproval({ ...asker, via: "dashboard" }, approval_id, { approve: { digest: approval } })))
        .toMatchObject({ ok: false });
      await runEffect(decideApproval(protector, approval_id, { approve: { digest: approval } }));
      expect(await as(asker, move, approval_id)).toMatchObject({ ok: true });
      expect(await rows()).toMatchObject([{ kind: "move" }]);
    });

    it("a Release asks someone who can protect it, not any approver", async () => {
      const { approval_id } = askedOf(await as(member, { kind: "release", args: {} }));

      expect(await review(approval_id)).toMatchObject({ protected: { environment: "production" } });
    });

    it("an approved Move to one Server moves to no other", async () => {
      machines = [...machines, { name: "fsn-3", pool: true, copy: null }];
      const { approval, approval_id } = askedOf(await as(member, { kind: "move", args: { to: "fsn-2" } }));
      await runEffect(decideApproval(protector, approval_id, { approve: { digest: approval } }));

      expect(await as(member, { kind: "move", args: { to: "fsn-3" } }, approval_id)).toMatchObject({ ok: false, refusal: { code: "approval_required" } });
      expect(await review(approval_id)).toMatchObject({ operation: { name: "data to fsn-2", preview: { to: { name: "fsn-2", machine: idOf("fsn-2") } } } });
      expect(await rows()).toEqual([]);
    });

    it("an approved Move to a Server by its id moves to no other Machine of that name", async () => {
      machines = [...machines, { name: "fsn-3", pool: true, copy: null }];
      const { approval, approval_id } = askedOf(await as(member, { kind: "move", args: { to: "fsn-3", machine: idOf("fsn-3") } }));
      await runEffect(decideApproval(protector, approval_id, { approve: { digest: approval } }));

      expect(await as(member, { kind: "move", args: { to: "fsn-3", machine: idOf("fsn-2") } }, approval_id)).toMatchObject({ ok: false });
      expect(await rows()).toEqual([]);
      expect(await review(approval_id)).toMatchObject({ operation: { preview: { to: { name: "fsn-3", machine: idOf("fsn-3") } } } });
    });

    it("a Restore names the Server it restores from", async () => {
      machines = [{ name: "fsn-1", pool: true, copy: null }, { name: "fsn-2", pool: true, copy: slotCopy(base) }];

      const { approval_id } = askedOf(await as(member, { kind: "restore", args: { from: "fsn-2" } }));

      expect(await review(approval_id)).toMatchObject({ operation: { name: "data from fsn-2", preview: { from: { name: "fsn-2", machine: idOf("fsn-2") } } }, protected: { environment: "production" } });
    });

    it("a member can't decide another member's Release", async () => {
      const { approval, approval_id } = askedOf(await as(member, { kind: "release", args: {} }));
      const peer: Caller = { ...member, userId: protectorId, via: "dashboard" };

      expect(await runEffect(decideApproval(peer, approval_id, { approve: { digest: approval } }))).toMatchObject({ ok: false });
    });

    it("a member's Move asks even when its first step seals an older second primary, and its approval covers that Server only", async () => {
      machines = [
        { name: "fsn-1", pool: true, copy: writerCopy(base, 3) },
        { name: "fsn-2", pool: true, copy: writerCopy(snap("20"), 5) },
        { name: "fsn-3", pool: true, copy: null },
        { name: "fsn-4", pool: true, copy: null },
      ];
      const { approval, approval_id } = askedOf(await as(member, { kind: "move", args: { to: "fsn-3" } }));
      await runEffect(decideApproval(protector, approval_id, { approve: { digest: approval } }));

      expect(await as(member, { kind: "move", args: { to: "fsn-4" } }, approval_id)).toMatchObject({ ok: false, refusal: { code: "approval_required" } });
      expect(await rows()).toEqual([]);
      expect(await as(member, { kind: "move", args: { to: "fsn-3" } }, approval_id)).toMatchObject({ ok: true });
    });

    it("a member seals an older second primary unasked, as Ployz's own sync would", async () => {
      machines = [{ name: "fsn-1", pool: true, copy: writerCopy(base, 3) }, { name: "fsn-2", pool: true, copy: writerCopy(snap("20"), 5) }];

      expect(await as(member, { kind: "sync", args: { full: true } })).toMatchObject({ ok: true });
      await harness.pool.query("truncate table volume_run cascade");
      expect(await as(member, { kind: "mirror", args: { to: "fsn-2" } })).toMatchObject({ ok: true });
    });

    describe("a grow", () => {
      const serves = [requirements.volumeFloor.capability, requirements.volumeSize.capability, requirements.volumeShrink.capability];
      const grow = (gigabytes: number): VolumeRunInput => ({ kind: "resize", args: { maximum_bytes: gigabytes * GB } });

      beforeEach(() => {
        storeVolumes = [{ id: volumeId, name: "data", storage: { kind: "provisioned", maximumBytes: 10 * GB }, recorded_storage: 10 * GB }];
        machines = [{ name: "fsn-1", pool: true, copy: writerCopy(), capabilities: serves, bound: 10 * GB }];
      });

      it.each(askers)("%s's grow asks someone who can protect it, for that size only", async (_, asker) => {
        const admitted = await as(asker, grow(20));
        const { approval, approval_id } = askedOf(admitted);

        expect(admitted).toMatchObject({ refusal: { details: { protected: { environment: "production" } } } });
        expect(await review(approval_id)).toMatchObject({ operation: { verb: "resize", name: "data from 10 GB to 20 GB", preview: { recorded_bytes: 10 * GB, maximum_bytes: 20 * GB } } });
        expect(await rows()).toEqual([]);
        await runEffect(decideApproval(protector, approval_id, { approve: { digest: approval } }));
        expect(await as(asker, grow(30), approval_id)).toMatchObject({ ok: false, refusal: { code: "approval_required" } });
        expect(await as(asker, grow(20), approval_id)).toMatchObject({ ok: true });
        expect(await rows()).toMatchObject([{ kind: "resize" }]);
      });

      it("a member's grow from the dashboard runs as they asked once someone who can protect it approves", async () => {
        await harness.pool.query("insert into member (user_id, organization_id, role) values ($1, $2, 'member')", [userId, organizationId]);
        const { approval, approval_id } = askedOf(await as({ ...member, via: "dashboard" }, grow(20)));

        expect(await runEffect(decideAndApply(protector, approval_id, { approve: { digest: approval } }))).toMatchObject({ ok: true });
        const ran = await harness.pool.query<{ args: unknown; requested_by_user_id: string }>("select args, requested_by_user_id from volume_run");
        expect(ran.rows).toEqual([{ args: { maximum_bytes: 20 * GB }, requested_by_user_id: userId }]);
      });

      it("an approved grow asks again once the Volume's size moved, so it never runs as a shrink", async () => {
        const { approval, approval_id } = askedOf(await as(member, grow(20)));
        storeVolumes = [{ id: volumeId, name: "data", storage: { kind: "provisioned", maximumBytes: 30 * GB }, recorded_storage: 30 * GB }];
        await runEffect(decideApproval(protector, approval_id, { approve: { digest: approval } }));

        expect(await as(member, grow(20), approval_id)).toMatchObject({ ok: false, refusal: { code: "approval_required" } });
        expect(await rows()).toEqual([]);
      });

      it("a grow asks even when its first step seals an older second primary", async () => {
        machines = [
          { name: "fsn-1", pool: true, copy: writerCopy(base, 3), capabilities: serves, bound: 10 * GB },
          { name: "fsn-2", pool: true, copy: writerCopy(snap("20"), 5), capabilities: serves, bound: 10 * GB },
        ];

        expect(await as(member, grow(20))).toMatchObject({ ok: false, refusal: { code: "approval_required" } });
        expect(await rows()).toEqual([]);
      });

      it("someone who can protect it grows unasked", async () => {
        expect(await as({ ...caller, via: "cli" }, grow(20))).toMatchObject({ ok: true });
      });
    });

    it("someone who can protect it moves unasked, and a member still mirrors unasked", async () => {
      expect(await as({ ...caller, via: "cli" }, { kind: "move", args: { to: "fsn-2" } })).toMatchObject({ ok: true });
      await harness.pool.query("truncate table volume_run cascade");
      expect(await as(member, { kind: "mirror", args: { to: "fsn-2" } })).toMatchObject({ ok: true });
    });

    it("an unprotected Environment lets a member move unasked", async () => {
      storeProtected = false;

      expect(await as(member, { kind: "move", args: { to: "fsn-2" } })).toMatchObject({ ok: true });
    });
  });

  describe("admission", () => {
    const run = admit;

    it("a Mirror Remove of the last copy asks first, names the copy it deletes, and writes no run", async () => {
      machines = [
        { name: "fsn-1", pool: true, copy: null },
        { name: "fsn-2", pool: true, copy: slotCopy(base) },
      ];

      const admitted = await run({ kind: "delete_mirror", args: { slot: null, confirmed_name: "data", role: "any" } });

      expect(admitted).toMatchObject({
        ok: false,
        refusal: { code: "approval_required", details: { effects: [{ kind: "deletes_volume", node: `${idOf("fsn-2")}/${dockerVolume}` }] } },
      });
      expect(await rows()).toEqual([]);
      expect(send).not.toHaveBeenCalled();
    });

    it("mirror rm --if-old-copy is refused once its old copy became the mirror, and writes no run", async () => {
      machines = [
        { name: "fsn-1", pool: true, copy: writerCopy() },
        { name: "fsn-2", pool: true, copy: slotCopy(base, null, "7", "mirror") },
      ];

      const admitted = await run({ kind: "delete_mirror", args: { slot: "fsn-2", confirmed_name: null, role: "old_copy" } });

      expect(admitted).toEqual({ ok: false, refusal: { code: "conflict", message: "data-fsn-2 is no longer an old copy, so nothing was deleted", details: null } });
      expect(await rows()).toEqual([]);
    });

    it("mirror rm --if-old-copy of a real old copy starts its delete, named for an old copy", async () => {
      machines = [
        { name: "fsn-1", pool: true, copy: writerCopy() },
        { name: "fsn-2", pool: true, copy: slotCopy(base, null, "7", "old_copy") },
      ];

      const admitted = await run({ kind: "delete_mirror", args: { slot: "fsn-2", confirmed_name: null, role: "old_copy" } });

      expect(admitted).toMatchObject({ ok: true });
      expect((await harness.pool.query("select kind, state, args from volume_run")).rows)
        .toMatchObject([{ kind: "delete_mirror", state: "requested", args: { slot: "fsn-2", role: "old_copy" } }]);
    });

    it("a Mirror Remove on a busy Volume is refused as busy before it asks", async () => {
      machines = [
        { name: "fsn-1", pool: true, copy: null },
        { name: "fsn-2", pool: true, copy: slotCopy(base) },
      ];
      const busy = await requested({ kind: "sync", args: { full: false } });
      send.mockClear();

      const admitted = await run({ kind: "delete_mirror", args: { slot: null, confirmed_name: "data", role: "any" } });

      expect(admitted).toEqual({
        ok: false,
        refusal: { code: "conflict", message: `data is busy with a mirror refresh; ployz volume runs data ${busy.id} follows it, and ployz volume release data stops it`, details: { run: expect.objectContaining({ id: busy.id }) } },
      });
      expect((await rows()).map((row) => row.id)).toEqual([busy.id]);
      expect(send).not.toHaveBeenCalled();
    });

    it("a Release of a cut root with no snapshot is refused before any row, with the move to run again", async () => {
      named("fsn-1").copy = writerCopy(null, 0, true);

      const admitted = await run({ kind: "release", args: {} });

      expect(admitted).toMatchObject({
        ok: false,
        refusal: { code: "conflict", message: "data on fsn-1 stopped before its first snapshot; ployz volume move data --to <another server>" },
      });
      expect(await rows()).toEqual([]);
      expect(send).not.toHaveBeenCalled();
    });

    it("a Move onto a Server without the generation floor is refused before any row, naming the Server", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: null, capabilities: [requirements.volumeMove.capability] };

      const admitted = await run({ kind: "move", args: { to: "fsn-2" } });

      expect(admitted).toMatchObject({ ok: false, refusal: { message: "Volume move is not available on fsn-2." } });
      expect(await rows()).toEqual([]);
      expect(send).not.toHaveBeenCalled();
    });

    it("a Move off a writer without the generation floor is refused before any row, naming the writer", async () => {
      first(machines).capabilities = [requirements.volumeMove.capability];

      const admitted = await run({ kind: "move", args: { to: "fsn-2" } });

      expect(admitted).toMatchObject({ ok: false, refusal: { message: "Volume move is not available on fsn-1." } });
      expect(await rows()).toEqual([]);
    });

    it("a Server without the floor that holds no copy and receives none does not hold a Move back", async () => {
      machines.push({ name: "fsn-3", pool: true, copy: null, capabilities: [requirements.volumeMove.capability] });

      expect(await run({ kind: "move", args: { to: "fsn-2" } })).toMatchObject({ ok: true });
    });

    it("a Move onto the writer beside a mirror synced to its final snapshot, with no stopped Container, is refused", async () => {
      const G = newSnap("f-1");
      machines = [
        { name: "fsn-1", pool: true, copy: slotCopy(G) },
        { name: "fsn-2", pool: true, copy: writerCopy(G, 2) },
      ];
      containers = [{ machine: "fsn-2", id: "created-1", running: true }];

      const admitted = await run({ kind: "move", args: { to: "fsn-2" } });

      expect(admitted).toMatchObject({ ok: false, refusal: { message: "data's writer is already on fsn-2" } });
      expect(await rows()).toEqual([]);
    });

    describe("a Release from slots", () => {
      const slots = () => {
        const at = newSnap("f-1");
        machines = [
          { name: "fsn-1", pool: true, copy: slotCopy(at) },
          { name: "fsn-2", pool: true, copy: slotCopy(at) },
        ];
        containers = [{ machine: "fsn-1", id: "web-fsn-1", running: false }];
      };
      const release = (approvalId: string | null = null) =>
        runEffect(runVolume(caller, { volumeId, environment, kind: "release", args: {} }, { key: null, approvalId }));
      const approve = async (asked: Awaited<ReturnType<typeof release>>) => {
        if (asked.ok || asked.refusal.code !== "approval_required") throw new Error(`expected an ask, got ${JSON.stringify(asked)}`);
        // SAFETY: an approval_required refusal carries the digest and id of the approval it recorded.
        const { approval, approval_id } = asked.refusal.details as { approval: string; approval_id: string };
        await runEffect(decideApproval({ ...caller, via: "dashboard" }, approval_id, { approve: { digest: approval } }));
        return release(approval_id);
      };
      const admittedId = (admitted: Awaited<ReturnType<typeof release>>) => {
        if (!admitted.ok) throw new Error(admitted.refusal.message);
        return admitted.id;
      };
      const changed = (holder: string) =>
        `data's copies changed since this release was planned; nothing was forgotten and data's Service stays stopped on ${holder}. Run ployz volume release data again`;

      it("whose kept-source read times out asks for every slot it may forget, and the approved run forgets only the copy beside the source", async () => {
        slots();
        frameFault = (read) => read % 2 === 0;

        const asked = await release();

        expect(asked).toMatchObject({
          ok: false,
          refusal: { code: "approval_required", details: { effects: [{ node: `${idOf("fsn-1")}/${dockerVolume}` }, { node: `${idOf("fsn-2")}/${dockerVolume}` }] } },
        });
        expect(await rows()).toEqual([]);
        expect(send).not.toHaveBeenCalled();

        const id = admittedId(await approve(asked));
        frameFault = null;
        const output = await execute(id);

        expect(output.result).toEqual({ runId: id, released: "fsn-1" });
        expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2", "forget@fsn-2", "writer@fsn-1(kept)"]);
      });

      it("planned with two stopped Containers forgets nothing once one of them goes", async () => {
        slots();
        containers.push({ machine: "fsn-2", id: "web-fsn-2", running: false });
        const id = admittedId(await release());
        containers = containers.filter((container) => container.machine !== "fsn-2");

        const output = await execute(id);

        expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2"]);
        expect(output.error).toBeDefined();
        expect(machines.map((fake) => fake.copy?.kind)).toEqual(["slot", "slot"]);
        expect(containers).toEqual([{ machine: "fsn-1", id: "web-fsn-1", running: false }]);
        expect(await rows()).toMatchObject([{ state: "failed", message: changed("fsn-1") }]);
      });

      it("approved to keep fsn-1 fails rather than forget fsn-1 when fsn-2 holds the stopped Container instead", async () => {
        slots();
        const id = admittedId(await approve(await release()));
        containers = [{ machine: "fsn-2", id: "web-fsn-2", running: false }];

        const output = await execute(id);

        expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2"]);
        expect(output.error).toBeDefined();
        expect(machines.map((fake) => fake.copy?.kind)).toEqual(["slot", "slot"]);
        expect(await rows()).toMatchObject([{ state: "failed", message: changed("fsn-2") }]);
      });
    });
  });

  describe("orphan detection", () => {
    const copies = (...entries: Array<[string, string, "writer" | "slot" | "switching"]>) =>
      entries.map(([machine, name, role]) => ({ machine_id: idOf(machine), name, role })) as CopyObservation["copies"];

    it("finds slots no config entry holds and no Machine writes", () => {
      expect(orphanSlotNames(copies(
        ["fsn-1", dockerVolume, "writer"],
        ["fsn-2", dockerVolume, "slot"],
        ["fsn-2", "gone_vol-a", "slot"],
        ["fsn-3", "gone_vol-a", "slot"],
        ["fsn-2", "moving_vol-b", "slot"],
        ["fsn-3", "moving_vol-b", "switching"],
        ["fsn-2", "kept_vol-c", "slot"],
      ), new Set([dockerVolume, "kept_vol-c"]))).toEqual(["gone_vol-a"]);
    });

    it("starts one orphan run per name, skipping config names and a name already running", async () => {
      const observe = () => Effect.succeed(copies(
        ["fsn-2", dockerVolume, "slot"],
        ["fsn-2", "gone-production_vol-a", "slot"],
        ["fsn-2", "gone-production_vol-b", "slot"],
      ));

      const first = await runEffect(startOrphanDeletes(organizationId, observe).pipe(Effect.orDie));
      const again = await runEffect(startOrphanDeletes(organizationId, observe).pipe(Effect.orDie));

      expect(first).toHaveLength(2);
      expect(again).toEqual([]);
      expect((await rows()).map((row) => [row.docker_volume, row.orphan, row.kind])).toEqual([
        ["gone-production_vol-a", true, "delete_mirror"],
        ["gone-production_vol-b", true, "delete_mirror"],
      ]);
      expect(send).toHaveBeenCalledTimes(2);
      expect(send).toHaveBeenCalledWith(expect.objectContaining({
        id: `volume-run-${first[0]}`,
        data: expect.objectContaining({ organizationId, volumeId: "a", runId: first[0] }),
      }));
    });

    it("starts a run for a seed root Docker never adopted only when no config entry holds its name", async () => {
      const answers = new Map([
        [dockerVolume, [member("fsn-1", "seed")]],
        ["gone-production_vol-s", [member("fsn-1", "seed")]],
        ["gone-production_vol-w", [member("fsn-1", "seed"), member("fsn-2", "writer")]],
        ["gone-production_vol-o", [member("fsn-1", "rootWithoutSeed")]],
      ]);
      const inspect = vi.fn((_: string, name: string) => Effect.succeed(answers.get(name) ?? [member("fsn-1", "empty")]));
      const observe = () => Effect.succeed(copies(
        ["fsn-1", dockerVolume, "writer"],
        ["fsn-1", "gone-production_vol-s", "writer"],
        ["fsn-1", "gone-production_vol-w", "writer"],
        ["fsn-1", "gone-production_vol-o", "switching"],
      ));

      await runEffect(startOrphanDeletes(organizationId, observe, inspect).pipe(Effect.orDie));

      expect((await rows()).map((row) => [row.docker_volume, row.orphan, row.kind])).toEqual([
        ["gone-production_vol-s", true, "delete_mirror"],
      ]);
      expect(inspect).not.toHaveBeenCalledWith(organizationId, dockerVolume);
    });

    it("an orphan run destroys a seed root Docker never adopted", async () => {
      machines = [{ name: "fsn-1", pool: true, copy: seedCopy }];
      const observe = () => Effect.succeed(copies(["fsn-1", "gone-production_vol-a", "writer"]));
      const inspect = () => Effect.succeed([member("fsn-1", "seed")]);

      const [runId] = await runEffect(startOrphanDeletes(organizationId, observe, inspect).pipe(Effect.orDie));
      if (runId === undefined) throw new Error("no orphan run started");
      const output = await execute(runId, "run-1", undefined, "a");

      expect(output.result).toEqual({ runId, destroyed: ["fsn-1"] });
      expect(verbs).toEqual(["forget@fsn-1"]);
      expect(machines[0]?.copy).toBeNull();
      expect(await rows()).toMatchObject([{ state: "done" }]);
    });

    it("keeps the slot of a Volume whose removal is staged but not yet deployed", async () => {
      storeVolumes = [{ id: volumeId, name: "data", storage: { kind: "provisioned", maximumBytes: 5_000_000 }, change: "delete" }];

      const started = await runEffect(startOrphanDeletes(organizationId, () => Effect.succeed(copies(["fsn-2", dockerVolume, "slot"]))).pipe(Effect.orDie));

      expect(started).toEqual([]);
      expect(await rows()).toEqual([]);
    });
  });
  describe("a Deploy on a busy Volume", () => {
    const DEPLOY = "deploy-1";
    const A = { id: idOf("fsn-1"), name: "fsn-1" } as const;
    type Lane = { id: string; kind: string; state: string; phase: string; message: string | null; gen: string; inngest_run_id: string | null; args: VolumeRunInput["args"] };
    const lanes = async () => (await harness.pool.query(
      "select id, kind, state, phase, message, gen, inngest_run_id, args from volume_run order by gen")).rows as Lane[];
    const moving = async (phase: "warm" | "committed" = "warm") => {
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });
      await harness.pool.query("update volume_run set state = 'running', inngest_run_id = 'run-move', epoch = 1, phase = $2, current = $3 where id = $1",
        [run.id, phase, phase === "committed" ? idOf("fsn-2") : null]);
      send.mockClear();
      return run;
    };
    const place = () => runEffect(placeDeployVolumes(organizationId, "env-1", "d-1", DEPLOY));
    const fence = (lane: DeployLane | undefined) => {
      if (lane === undefined) throw new Error("the Deploy placed no lane");
      return runEffect(fenceDeployVolume(lane, DEPLOY));
    };
    const finish = () => runEffect(finishDeployVolumes(DEPLOY, "done"));
    const afterCut = () => {
      const cut = newSnap("f-1");
      machines = [{ name: "fsn-1", pool: true, copy: slotCopy(cut) }, { name: "fsn-2", pool: true, copy: slotCopy(cut) }];
      containers = [{ machine: "fsn-1", id: "web-fsn-1", running: false }];
      return cut;
    };

    it("during a warm move: the move yields, the Deploy's row takes the next gen, the Floor goes to A before anything else, the app stays on A, and the move is requested again once", async () => {
      const taken = await moving();

      const placed = await place();

      expect(placed).toMatchObject([{ kind: "took", run: { kind: "deploy", volumeId }, resume: { to: "fsn-2" } }]);
      const [old, mine] = await lanes();
      expect(old).toMatchObject({ id: taken.id, state: "yielded", message: "a Deploy took this run over" });
      expect(mine).toMatchObject({ kind: "deploy", state: "running", inngest_run_id: DEPLOY, args: { deployment: "d-1", resume: { to: "fsn-2" } } });
      expect(Number(mine?.gen)).toBe(Number(old?.gen) + 1);
      expect(send).toHaveBeenCalledWith(expect.objectContaining({ name: "volume/run.taken", data: { runId: taken.id } }));

      expect(await fence(placed[0])).toEqual({ on: "fsn-1" });
      expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2"]);
      expect(sent[0]).toMatchObject({ want: { kind: "floor" }, gen: Number(mine?.gen) });
      expect(sent[1]).toMatchObject({ expect: { kind: "none" }, want: { kind: "floor" }, gen: Number(mine?.gen) });
      expect(containers).toEqual([{ machine: "fsn-1", id: "web-fsn-1", running: true }]);

      send.mockClear();
      expect(await finish()).toEqual({ ended: 1, requeued: [expect.any(String)], refused: [] });
      expect(await lanes()).toMatchObject([
        { id: taken.id, state: "yielded" },
        { kind: "deploy", state: "done" },
        { kind: "move", state: "requested", args: { to: "fsn-2" } },
      ]);
      expect(send).toHaveBeenCalledTimes(1);
      expect(send).toHaveBeenCalledWith(expect.objectContaining({ name: "volume/run.requested" }));
      expect(await finish()).toEqual({ ended: 0, requeued: [], refused: [] });
      expect((await lanes()).filter((lane) => lane.kind === "move")).toHaveLength(2);
    });

    it("a dead move after the takeover is stale on A and its commit is refused, so B never starts the app", async () => {
      const taken = await moving();
      const [placed] = await place();
      await fence(placed);
      const [old] = await lanes();
      const dead: RunContext = { ...context(taken.id), kind: "move", args: { to: "fsn-2" }, gen: Number(old?.gen) };

      const cut = await runEffect(askCopy(dead, A, { kind: "writer", epoch: 0 }, { kind: "snapshot", name: "f-1" }));

      expect(cut).toMatchObject({ ok: false, reason: "stale" });
      expect(await runEffect(commitMove(dead, "run-move", idOf("fsn-2")))).toBe(false);
      expect(await runEffect(commitAndStart(dead, "run-move", { to: { id: idOf("fsn-2"), name: "fsn-2" }, from: null, expect: { kind: "none" }, at: base.guid, epoch: 1 })))
        .toMatchObject({ ok: false, reason: "stale" });
      expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2", "snapshot@fsn-1(f-1)"]);
      expect(containers).toEqual([{ machine: "fsn-1", id: "web-fsn-1", running: true }]);
      expect(machines[0]?.copy).toMatchObject({ kind: "root", newest: { guid: base.guid } });
    });

    it("a warm move's late mirror to B after the takeover is stale on B, so B keeps the copy the fence saw", async () => {
      const next = newSnap("w-1-1");
      named("fsn-1").copy = writerCopy(next, 1);
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(base) };
      const taken = await moving();
      const [placed] = await place();

      expect(await fence(placed)).toEqual({ on: "fsn-1" });
      const [old] = await lanes();
      const dead: RunContext = { ...context(taken.id), kind: "move", args: { to: "fsn-2" }, gen: Number(old?.gen) };
      const late = await runEffect(askCopy(dead, { id: idOf("fsn-2"), name: "fsn-2" }, { kind: "mirror", newest: base.guid, slot: "1" }, { kind: "mirror", from: "fdcc::1", target: next.name, drain: 3_000 }));
      inspect(named("fsn-2"));

      expect(late).toMatchObject({ ok: false, reason: "stale" });
      expect(named("fsn-2").copy).toEqual(slotCopy(base));
      expect(containers).toEqual([{ machine: "fsn-1", id: "web-fsn-1", running: true }]);
    });

    it("a Server without the floor that holds no copy is left unfloored, so the fence still deploys on A", async () => {
      machines.push({ name: "fsn-3", pool: true, copy: null, capabilities: [requirements.volumeMove.capability] });
      failNext.set("floor@fsn-3", new RawFault({ code: "invalid_argument", message: "unknown variant `floor`", details: null }));
      await moving();
      const [placed] = await place();

      expect(await fence(placed)).toEqual({ on: "fsn-1" });
      expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2"]);
      expect(containers).toEqual([{ machine: "fsn-1", id: "web-fsn-1", running: true }]);
    });

    it.each(["down", "busy"] as const)("a Machine whose floor is %s stops none of the others: the Servers after it are floored, then the fence fails on it", async (fault) => {
      machines.push({ name: "fsn-3", pool: true, copy: null });
      failNext.set("floor@fsn-2", fault);
      await moving();
      const [placed] = await place();
      const [, mine] = await lanes();

      await expect(fence(placed)).rejects.toThrow(/fsn-2/);

      expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2", "floor@fsn-3"]);
      expect(machines.map((machine) => machine.floor)).toEqual([Number(mine?.gen), undefined, Number(mine?.gen)]);
    });

    it("after the cut finished but before the floor rose: the reply is a slot, and the promote starts the stopped app on A", async () => {
      const cut = afterCut();
      await moving();
      const [placed] = await place();

      expect(await fence(placed)).toEqual({ on: "fsn-1" });

      expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2", "writer@fsn-1(kept)"]);
      expect(sent[2]).toMatchObject({
        expect: { kind: "mirror", newest: cut.guid },
        want: { kind: "writer", at: cut.guid, epoch: 2, start: { kind: "kept", container_id: "web-fsn-1" } },
        gen: sent[0]?.gen,
      });
      expect(machines[0]?.copy).toMatchObject({ kind: "root", readonly: false, epoch: 2, newest: { guid: cut.guid } });
      expect(machines[1]?.copy).toMatchObject({ kind: "slot" });
      expect(containers).toEqual([{ machine: "fsn-1", id: "web-fsn-1", running: true }]);
    });

    it("a cut that flips A between the observe and the floor: the promote still starts the stopped app on A", async () => {
      const cut = newSnap("f-1");
      machines = [{ name: "fsn-1", pool: true, copy: writerCopy(base, 1, true) }, { name: "fsn-2", pool: true, copy: slotCopy(cut) }];
      containers = [{ machine: "fsn-1", id: "web-fsn-1", running: false }];
      beforeSet.set("floor@fsn-1", () => { named("fsn-1").copy = slotCopy(cut); });
      await moving();
      const [placed] = await place();

      expect(await fence(placed)).toEqual({ on: "fsn-1" });

      expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2", "writer@fsn-1(kept)"]);
      expect(machines[0]?.copy).toMatchObject({ kind: "root", readonly: false, newest: { guid: cut.guid } });
      expect(containers).toEqual([{ machine: "fsn-1", id: "web-fsn-1", running: true }]);
    });

    it("on a committed lane: it follows to B with the move's gen and the app runs only on B", async () => {
      const cut = afterCut();
      const taken = await moving("committed");

      const placed = await place();

      expect(placed).toMatchObject([{ kind: "follow", run: { id: taken.id, kind: "move" }, to: "fsn-2", epoch: 1 }]);
      expect(await fence(placed[0])).toEqual({ on: "fsn-2" });
      expect(verbs).toEqual(["writer@fsn-2(create)"]);
      expect(sent[0]).toMatchObject({ want: { kind: "writer", at: cut.guid, epoch: 1 }, gen: Number((await lanes())[0]?.gen) });
      expect(containers.filter((container) => container.running)).toEqual([{ machine: "fsn-2", id: `created-${volumeId}-${cut.guid}`, running: true }]);
      expect(machines[0]?.copy).toMatchObject({ kind: "slot" });
      expect(await lanes()).toMatchObject([{ id: taken.id, kind: "move", state: "running" }]);
      expect(await finish()).toEqual({ ended: 0, requeued: [], refused: [] });
      expect(await fence(placed[0])).toEqual({ on: "fsn-2" });
      expect(verbs).toEqual(["writer@fsn-2(create)"]);
    });

    it("on a committed Move whose run failed: a Deploy that targets nothing on a Volume a mirror removal holds follows the commitment to B without waiting, and refuses to start B once a run admitted after that removal settled the Volume", async () => {
      const cut = afterCut();
      const taken = await moving("committed");
      await harness.pool.query("update volume_run set state = 'failed', finished_at = now() where id = $1", [taken.id]);
      const mirror = await requested({ kind: "delete_mirror", args: { slot: "fsn-3", confirmed_name: null, machine: idOf("fsn-3"), role: "any" } });
      deployNodes = [{ type: "service", id: "svc-api", name: "api", outcome: "pending", rows: [] }];

      const placed = await place();

      expect(placed).toMatchObject([{ kind: "follow", run: { id: taken.id, kind: "move" }, to: "fsn-2", machine: idOf("fsn-2"), epoch: 1 }]);
      await harness.pool.query("update volume_run set state = 'done', finished_at = now() where id = $1", [mirror.id]);
      const restored = await requested({ kind: "restore", args: { from: "fsn-1" } });
      await harness.pool.query("update volume_run set state = 'failed', epoch = 2, finished_at = now() where id = $1", [restored.id]);
      await expect(fence(placed[0])).rejects.toThrow("data changed since this Deploy placed it; Deploy again");
      expect(verbs).toEqual([]);

      await harness.pool.query("delete from volume_run where id = $1", [restored.id]);
      expect(await fence(placed[0])).toEqual({ on: "fsn-2" });
      expect(verbs).toEqual(["writer@fsn-2(create)"]);
      expect(containers.filter((container) => container.running)).toEqual([{ machine: "fsn-2", id: `created-${volumeId}-${cut.guid}`, running: true }]);
    });

    it("removing B clears a committed Move that fails with its release while the removal waits on another Volume, so nothing stays owed to B", async () => {
      afterCut();
      const taken = await moving("committed");
      const [other] = (await harness.pool.query<{ id: string }>(`
        insert into volume_run (organization_id, environment_id, volume_id, volume_name, docker_volume, refquota_bytes, kind, args, phase, current, state, finished_at)
        select organization_id, environment_id, 'vol2', 'logs', 'logs-vol', refquota_bytes, kind, args, phase, current, 'failed', now()
        from volume_run where id = $1 returning id`, [taken.id])).rows;
      const holder = await harness.pool.connect();
      try {
        await holder.query("begin");
        await holder.query("select id from volume_run where id = $1 for update", [other?.id]);
        const removing = runEffect(Effect.exit(releaseLanesOff(organizationId, { id: idOf("fsn-2"), name: "fsn-2" }, [])));
        await waitingOnLock();
        expect(await runEffect(closeVolumeRun("run-move", "failure"))).toBe(1);
        await harness.pool.query("update volume_run set state = 'running', inngest_run_id = 'run-release' where volume_id = $1 and kind = 'release'", [volumeId]);
        expect(await runEffect(closeVolumeRun("run-release", "failure"))).toBe(1);
        await holder.query("commit");

        await expect(removing).resolves.toMatchObject({ _tag: "Success", value: [] });
      } finally {
        holder.release(true);
      }
      expect((await harness.pool.query("select kind, state from volume_run where volume_id = $1 order by gen", [volumeId])).rows)
        .toEqual([{ kind: "move", state: "failed" }, { kind: "release", state: "failed" }]);
      expect((await harness.pool.query("select id, volume_id from volume_run where organization_id = $1 and current = $2 and state <> 'done'",
        [organizationId, idOf("fsn-2")])).rows).toEqual([]);
      expect(await runEffect(isVolumeOwed(organizationId, volumeId))).toBe(false);
      expect((await harness.pool.query("select current from volume_run where id = $1", [other?.id])).rows).toEqual([{ current: null }]);
    });

    describe("on a committed Move whose run failed", () => {
      const failedCommit = async () => {
        const cut = afterCut();
        const taken = await moving("committed");
        await harness.pool.query("update volume_run set state = 'failed', finished_at = now() where id = $1", [taken.id]);
        return cut;
      };

      it("removing B ends the commitment: the Volume is no longer owed and a Deploy follows nothing", async () => {
        await failedCommit();

        await runEffect(releaseLanesOff(organizationId, { id: idOf("fsn-2"), name: "fsn-2" }, []));
        machines = machines.filter((fake) => fake.name !== "fsn-2");

        expect(await runEffect(isVolumeOwed(organizationId, volumeId))).toBe(false);
        expect(await place()).toEqual([]);
        await finish();
      });

      it("removing B clears each Volume's commitment on its own, so a Deploy holding one and reaching for the other is no deadlock", async () => {
        await failedCommit();
        const [owedFirst] = (await harness.pool.query<{ id: string }>("select id from volume_run where volume_id = $1 and phase = 'committed'", [volumeId])).rows;
        const [owedSecond] = (await harness.pool.query<{ id: string }>(`
          insert into volume_run (organization_id, environment_id, volume_id, volume_name, docker_volume, refquota_bytes, kind, args, phase, current, state, finished_at)
          select organization_id, environment_id, 'vol2', 'logs', 'logs-vol', refquota_bytes, kind, args, phase, current, state, finished_at
          from volume_run where id = $1 returning id`, [owedFirst?.id])).rows;
        const deploy = await harness.pool.connect();
        try {
          await deploy.query("begin");
          await deploy.query("select id from volume_run where id = $1 for update", [owedSecond?.id]);
          const removing = runEffect(Effect.exit(releaseLanesOff(organizationId, { id: idOf("fsn-2"), name: "fsn-2" }, [])));
          await waitingOnLock();
          const reaching = deploy.query("select id from volume_run where id = $1 for update", [owedFirst?.id]);

          await expect(reaching).resolves.toMatchObject({ rowCount: 1 });
          await deploy.query("commit");
          await expect(removing).resolves.toMatchObject({ _tag: "Success", value: [] });
        } finally {
          deploy.release(true);
        }
        expect((await harness.pool.query("select current from volume_run where id = any($1)", [[owedFirst?.id, owedSecond?.id]])).rows).toEqual([{ current: null }, { current: null }]);
        expect(await requested({ kind: "move", args: { to: "fsn-3" } })).toMatchObject({ kind: "move", state: "requested" });
      });

      it("removing B ends a release that inherits the commitment once its unfinished rows stop naming B", async () => {
        await failedCommit();
        const release = await requested({ kind: "release", args: {} });
        await harness.pool.query("update volume_run set state = 'running', inngest_run_id = 'run-release', phase = 'warm', current = null where id = $1", [release.id]);
        // An admission that read the commitment before the clear commits right after it.
        await harness.pool.query(`
          create function admit_after_clear() returns trigger language plpgsql as $$ begin
            update volume_run set phase = 'committed', current = OLD.current where id = '${release.id}' and phase <> 'committed';
            return null;
          end $$;
          create trigger admit_after_clear after update on volume_run for each row
            when (OLD.current is not null and NEW.current is null) execute function admit_after_clear();`);
        try {
          await runEffect(releaseLanesOff(organizationId, { id: idOf("fsn-2"), name: "fsn-2" }, []));
        } finally {
          await harness.pool.query("drop trigger admit_after_clear on volume_run; drop function admit_after_clear();");
        }

        expect((await lanes()).filter((lane) => lane.id === release.id)).toMatchObject([{ state: "failed" }]);
        expect(await runEffect(isVolumeOwed(organizationId, volumeId))).toBe(false);
        expect((await harness.pool.query("select id from volume_run where current = $1", [idOf("fsn-2")])).rows).toEqual([]);
      });

      it("a mirror removal running on another Server does not stop a Deploy from following it", async () => {
        await failedCommit();
        await requested({ kind: "delete_mirror", args: { slot: "fsn-3", confirmed_name: null, machine: idOf("fsn-3"), role: "any" } });

        expect(await place()).toMatchObject([{ kind: "follow", to: "fsn-2", ended: true }]);
      });

      it("protects B by its id after a rename: removing its mirror by the new name is refused, naming the new name", async () => {
        await failedCommit();
        machines = machines.map((fake) => fake.name === "fsn-2" ? { ...fake, name: "web-2", id: idOf("fsn-2") } : fake);
        const owes = "data finishes on web-2; ployz volume release data finishes it now";
        const plan = (input: VolumeRunInput) => runEffect(planVolumeRun(organizationId, { volumeId, environment, ...input }));

        expect(await plan({ kind: "delete_mirror", args: { slot: "web-2", confirmed_name: null, role: "any" } })).toMatchObject({ ok: false, refusal: { message: owes } });
        expect(await request({ kind: "delete_mirror", args: { slot: "web-2", confirmed_name: null, role: "any" } })).toMatchObject({ ok: false, refusal: { message: owes } });
        expect(await request({ kind: "move", args: { to: "fsn-1" } })).toMatchObject({ ok: false, refusal: { message: owes } });
      });

      it("protects B by the id its plan observed: removing its mirror by the new name is refused while the Cluster stops answering", async () => {
        await failedCommit();
        machines = machines.map((fake) => fake.name === "fsn-2" ? { ...fake, name: "web-2", id: idOf("fsn-2") } : fake);
        const volume = { environmentId: "env-1", volumeName: "data", dockerVolume, refquotaBytes: 5_000_000, recordedBytes: 0 };
        const planned = await runEffect(planVolumeRun(organizationId, { volumeId, environment, kind: "delete_mirror", args: { slot: "web-2", confirmed_name: null, role: "any" } }));
        expect(planned).toMatchObject({ ok: false, refusal: { message: "data finishes on web-2; ployz volume release data finishes it now" } });
        frameFault = () => true;

        const asked = { kind: "delete_mirror", args: { slot: "web-2", confirmed_name: null, machine: idOf("fsn-2"), role: "any" } } satisfies VolumeRunInput;
        expect(await runEffect(insertRun({ userId, organizationId }, { volumeId, ...asked }, volume, crypto.randomUUID()))).toMatchObject({ ok: false, refusal: { code: "conflict" } });
        expect(await request({ kind: "delete_mirror", args: { slot: "web-2", confirmed_name: null, role: "any" } })).toMatchObject({ ok: false, refusal: { code: "conflict" } });
        expect((await lanes()).filter((lane) => lane.kind === "delete_mirror")).toEqual([]);
      });

      it("admits removing another mirror by the id its plan observed, reading no Server inside the admission", async () => {
        const cut = await failedCommit();
        machines.push({ name: "fsn-3", pool: true, copy: slotCopy(cut) });
        const volume = { environmentId: "env-1", volumeName: "data", dockerVolume, refquotaBytes: 5_000_000, recordedBytes: 0 };
        const planned = await runEffect(planVolumeRun(organizationId, { volumeId, environment, kind: "delete_mirror", args: { slot: "fsn-3", confirmed_name: "data", role: "any" } }));
        if (!planned.ok) throw new Error(planned.refusal.message);
        expect(planned.input).toMatchObject({ kind: "delete_mirror", args: { slot: "fsn-3", machine: idOf("fsn-3") } });
        frameFault = () => true;
        const read = frames;

        expect(await runEffect(insertRun({ userId, organizationId }, planned.input, volume, crypto.randomUUID()))).toMatchObject({ ok: true });
        expect(frames).toBe(read);
      });

      it("cuts outside B name the step that finishes the commitment, from a release and from a mirror removal, and that step is accepted", async () => {
        await failedCommit();
        const cut = newSnap("f-2");
        machines = [
          { name: "fsn-1", pool: true, copy: writerCopy(cut, 8, true) },
          { name: "fsn-2", pool: true, copy: slotCopy(cut) },
          { name: "fsn-3", pool: true, copy: writerCopy(cut, 9, true) },
          { name: "fsn-4", pool: true, copy: slotCopy(cut) },
        ];
        const advice = "data has unfinished cuts on fsn-1 and fsn-3, so release cannot finish it on fsn-2; ployz server rm fsn-1, then ployz server rm fsn-3, then ployz volume release data";
        const plan = (input: VolumeRunInput) => runEffect(planVolumeRun(organizationId, { volumeId, environment, ...input }));

        expect(await plan({ kind: "release", args: {} })).toMatchObject({ ok: false, refusal: { message: advice } });
        expect(await plan({ kind: "delete_mirror", args: { slot: "fsn-4", confirmed_name: null, role: "any" } })).toMatchObject({ ok: false, refusal: { message: advice } });

        machines = machines.filter((fake) => fake.name !== "fsn-1" && fake.name !== "fsn-3");
        expect(await plan({ kind: "release", args: {} })).toMatchObject({ ok: true, input: { kind: "release", args: { plan: { forward: { to: idOf("fsn-2") } } } } });
      });

      it("a writable root beside a cut outside B blocks a mirror removal with the writer the release names, not the cut advice", async () => {
        await failedCommit();
        const cut = newSnap("f-2");
        machines = [
          { name: "fsn-1", pool: true, copy: writerCopy(cut, 8, true) },
          { name: "fsn-2", pool: true, copy: slotCopy(cut) },
          { name: "fsn-3", pool: true, copy: writerCopy(cut, 9) },
          { name: "fsn-4", pool: true, copy: slotCopy(cut) },
        ];
        const writer = "data-fsn-3 is still a writer copy, so data can't finish on fsn-2; ployz volume restore data --from fsn-3 keeps fsn-3's data";
        const plan = (input: VolumeRunInput) => runEffect(planVolumeRun(organizationId, { volumeId, environment, ...input }));

        expect(await plan({ kind: "release", args: {} })).toMatchObject({ ok: false, refusal: { message: writer } });
        expect(await plan({ kind: "delete_mirror", args: { slot: "fsn-4", confirmed_name: null, role: "any" } })).toMatchObject({ ok: false, refusal: { message: writer } });
      });

      it("a Deploy targeting the Volume holds it beside the follow: a Move is refused as deploying, then as owed once the Deploy ends", async () => {
        await failedCommit();
        const owes = "data finishes on fsn-2; ployz volume release data finishes it now";

        const placed = await place();

        expect(placed).toMatchObject([{ kind: "follow", to: "fsn-2", ended: true }]);
        expect((await lanes()).filter((lane) => lane.kind === "deploy")).toMatchObject([{ state: "running", inngest_run_id: DEPLOY, args: { hold: true } }]);
        expect(await request({ kind: "move", args: { to: "fsn-3" } })).toMatchObject({ ok: false, refusal: { message: "data is deploying; the Deploy ends its run" } });
        expect(await runEffect(isVolumeOwed(organizationId, volumeId))).toBe(true);
        expect(await fence(placed[0])).toEqual({ on: "fsn-2" });
        expect(verbs).toEqual(["writer@fsn-2(create)"]);

        await finish();

        expect((await lanes()).filter((lane) => lane.kind === "deploy")).toEqual([]);
        expect(await runEffect(isVolumeOwed(organizationId, volumeId))).toBe(true);
        expect(await request({ kind: "move", args: { to: "fsn-3" } })).toMatchObject({ ok: false, refusal: { message: owes } });
      });

      it("a retried placement that finds its own hold still follows the commitment", async () => {
        await failedCommit();
        await place();

        expect(await place()).toMatchObject([{ kind: "follow", to: "fsn-2", ended: true }]);
        expect((await lanes()).filter((lane) => lane.kind === "deploy")).toHaveLength(1);
      });

      it("a Deploy that takes over another Deploy's hold still follows the commitment", async () => {
        await failedCommit();
        await place();

        expect(await runEffect(placeDeployVolumes(organizationId, "env-1", "d-2", "deploy-2"))).toMatchObject([{ kind: "follow", to: "fsn-2", ended: true }]);
      });

      it("a Deploy that targets nothing on the Volume holds the commitment it follows: a release or a restore is refused until the Deploy ends, and the follow starts B", async () => {
        await failedCommit();
        deployNodes = [{ type: "service", id: "svc-api", name: "api", outcome: "pending", rows: [] }];
        const deploying = { ok: false, refusal: { message: "data is deploying; the Deploy ends its run" } };

        const placed = await place();

        expect((await lanes()).filter((lane) => lane.kind === "deploy")).toMatchObject([{ state: "running", inngest_run_id: DEPLOY, args: { hold: true } }]);
        expect(await request({ kind: "release", args: {} })).toMatchObject(deploying);
        expect(await request({ kind: "restore", args: { from: "fsn-1" } })).toMatchObject(deploying);
        expect(await fence(placed[0])).toEqual({ on: "fsn-2" });
        expect(verbs).toEqual(["writer@fsn-2(create)"]);

        await finish();

        expect((await lanes()).filter((lane) => lane.kind === "deploy")).toEqual([]);
      });

      it("refuses to start B once an older row decides the Volume instead of the commitment it followed", async () => {
        afterCut();
        const restored = await requested({ kind: "restore", args: { from: "fsn-1" } });
        await harness.pool.query("update volume_run set state = 'failed', epoch = 2, finished_at = now() where id = $1", [restored.id]);
        const taken = await moving("committed");
        await harness.pool.query("update volume_run set state = 'failed', finished_at = now() where id = $1", [taken.id]);
        deployNodes = [{ type: "service", id: "svc-api", name: "api", outcome: "pending", rows: [] }];
        const placed = await place();

        await harness.pool.query("update volume_run set current = null where id = $1", [taken.id]);

        await expect(fence(placed[0])).rejects.toThrow("data changed since this Deploy placed it; Deploy again");
        expect(verbs).toEqual([]);
      });

      it("a retried release step whose release already ended, beside a newer owed release, requests nothing new", async () => {
        await failedCommit();
        const placed = await place();
        await finish();
        const release = () => runEffect(releaseFollowed("env-1", DEPLOY, placed));
        const ended = "update volume_run set state = 'failed', finished_at = now(), phase = 'committed', current = $2 where id = $1";
        const [first] = await release();
        await harness.pool.query(ended, [first, idOf("fsn-2")]);
        const newer = await requested({ kind: "release", args: {} });
        await harness.pool.query(ended, [newer.id, idOf("fsn-2")]);

        expect(await release()).toEqual([first]);
        expect((await lanes()).filter((lane) => lane.kind === "release")).toMatchObject([{ id: first, state: "failed" }, { id: newer.id, state: "failed" }]);
      });

      it("the release takes the lane as the Deploy's hold drops, so a mirror removal asked next waits on the release", async () => {
        await failedCommit();
        const placed = await place();

        const [released] = await runEffect(releaseFollowed("env-1", DEPLOY, placed));

        expect((await lanes()).filter((lane) => lane.kind !== "move")).toMatchObject([{ id: released, kind: "release", state: "requested", args: { follows: idOf("fsn-2") } }]);
        expect(await request({ kind: "delete_mirror", args: { slot: "fsn-3", confirmed_name: null, machine: idOf("fsn-3"), role: "any" } }))
          .toMatchObject({ ok: false, busy: expect.objectContaining({ id: released, kind: "release" }) });
      });

      it("a mirror removal the Deploy stamped beside it no longer counts as deploying once the release is asked", async () => {
        await failedCommit();
        const mirror = await requested({ kind: "delete_mirror", args: { slot: "fsn-3", confirmed_name: null, machine: idOf("fsn-3"), role: "any" } });
        const placed = await place();
        await harness.pool.query("update volume_run set state = 'done', finished_at = now() where id = $1", [mirror.id]);

        const [released] = await runEffect(releaseFollowed("env-1", DEPLOY, placed));

        expect(released).toBeDefined();
        expect((await harness.pool.query("select id from volume_run where held_by is not null")).rows).toEqual([]);
      });

      it.each([
        ["finished on B", "done", "fsn-2"],
        ["handed to another Server", "failed", "fsn-3"],
      ] as const)("a commitment %s before the Deploy's release step gets no release from it", async (_, state, current) => {
        await failedCommit();
        const placed = await place();
        await finish();
        const settled = await requested({ kind: "release", args: {} });
        await harness.pool.query("update volume_run set state = $2, finished_at = now(), phase = 'committed', current = $3 where id = $1", [settled.id, state, idOf(current)]);

        const volume = { environmentId: "env-1", volumeName: "data", dockerVolume, refquotaBytes: 5_000_000, recordedBytes: 0 };
        const follow = { volumeId, kind: "release" as const, args: { follows: idOf("fsn-2") } };

        expect(await runEffect(releaseFollowed("env-1", DEPLOY, placed))).toEqual([]);
        expect((await lanes()).filter((lane) => lane.kind === "release")).toMatchObject([{ id: settled.id, state }]);
        expect(await runEffect(insertRun({ userId: null, organizationId }, follow, volume, crypto.randomUUID())))
          .toMatchObject({ ok: false, refusal: { code: "conflict", details: { reason: "follow_settled" } } });
      });

      describe("once a release admitted after a mirror removal the Deploy placed beside took the commitment over and floored B at its newer gen", () => {
        const successor = async (state: "running" | "done") => {
          await failedCommit();
          const mirror = await requested({ kind: "delete_mirror", args: { slot: "fsn-3", confirmed_name: null, machine: idOf("fsn-3"), role: "any" } });
          deployNodes = [{ type: "service", id: "svc-api", name: "api", outcome: "pending", rows: [] }];
          const placed = await place();
          await harness.pool.query("update volume_run set state = 'done', finished_at = now() where id = $1", [mirror.id]);
          const release = await requested({ kind: "release", args: {} });
          const { rows: [floor] } = await harness.pool.query<{ gen: string }>(
            "update volume_run set state = $2, phase = 'committed', current = $3, epoch = 1 where id = $1 returning gen", [release.id, state, idOf("fsn-2")]);
          for (const fake of machines) fake.floor = Number(floor?.gen);
          return placed;
        };

        it("a release that finished leaves B the writer with no app running, and the Deploy starts on B with no request", async () => {
          const placed = await successor("done");
          named("fsn-2").copy = writerCopy(machines[1]?.copy?.newest ?? null, 1, false);

          expect(await fence(placed[0])).toEqual({ on: "fsn-2" });
          expect(verbs).toEqual([]);
          expect(containers.filter((container) => container.running)).toEqual([]);
        });

        it("a release that makes B the writer while the Deploy asks is read again, and the Deploy starts on B", async () => {
          const placed = await successor("running");
          beforeSet.set("writer@fsn-2", () => { named("fsn-2").copy = writerCopy(machines[1]?.copy?.newest ?? null, 1, false); });

          expect(await fence(placed[0])).toEqual({ on: "fsn-2" });
          expect(verbs).toEqual(["writer@fsn-2(create)"]);
        });

        it("a release still running fails the Deploy with what to do", async () => {
          const placed = await successor("running");

          await expect(fence(placed[0])).rejects.toThrow(new NonRetriableError("data is finishing its move on fsn-2; Deploy again"));
          expect(verbs).toEqual(["writer@fsn-2(create)"]);
          expect(named("fsn-2").copy).toMatchObject({ kind: "slot" });
        });
      });

      it("a silent B fails the follow with what to do", async () => {
        await failedCommit();
        const placed = await place();
        machines = machines.filter((fake) => fake.name !== "fsn-2");

        await expect(fence(placed[0])).rejects.toThrow(
          "data finishes on fsn-2, which did not answer. Bring fsn-2 back or forget it with ployz server forget fsn-2 --confirm dead. Restore from fsn-1's copy: ployz volume restore data --from fsn-1");
      });

      it("a copyless B fails the follow with what to do", async () => {
        await failedCommit();
        const placed = await place();
        named("fsn-2").copy = null;

        await expect(fence(placed[0])).rejects.toThrow(
          "data finishes on fsn-2, which holds no copy to start. Remove it with ployz server rm fsn-2. Restore from fsn-1's copy: ployz volume restore data --from fsn-1");
      });
    });

    describe("after a Deploy followed the commitment, its placement removed A's Container, and the Service on B was stopped", () => {
      let cut: Snapshot;
      const followedThenStopped = async () => {
        cut = afterCut();
        const taken = await moving("committed");
        await harness.pool.query("update volume_run set state = 'failed', finished_at = now() where id = $1", [taken.id]);
        await fence((await place())[0]);
        await finish();
        containers = containers.filter((container) => container.machine !== "fsn-1");
        for (const container of containers) container.running = false;
        verbs = [];
      };

      it("a Deploy follows it again, keeps B the writer and starts nothing", async () => {
        await followedThenStopped();

        const placed = await place();

        expect(placed).toMatchObject([{ kind: "follow", to: "fsn-2" }]);
        expect(await fence(placed[0])).toEqual({ on: "fsn-2" });
        expect(verbs).toEqual([]);
        expect(containers).toEqual([{ machine: "fsn-2", id: `created-${volumeId}-${cut.guid}`, running: false }]);
      });

      it("a release settles it done", async () => {
        await followedThenStopped();
        const id = (await requested({ kind: "release", args: {} })).id;

        const output = await execute(id, "run-3");

        expect(output.result).toMatchObject({ moved: "fsn-2" });
        expect(verbs).toEqual(["writer@fsn-2(none)"]);
        expect((await rows()).at(-1)).toMatchObject({ id, kind: "release", state: "done" });
      });
    });

    it("a Deploy that follows an ended commitment requests the release that settles it, once, and a Move is admitted after", async () => {
      afterCut();
      const taken = await moving("committed");
      await harness.pool.query("update volume_run set state = 'failed', finished_at = now() where id = $1", [taken.id]);
      const deploy = () => new InngestTestEngine({
        function: createRunStoreDeployment(new Inngest({ id: "test" }), runEffect as Parameters<typeof createRunStoreDeployment>[1]),
        events: [{ name: "config/deployment.admitted", data: { organizationId, environmentId: "env-1", deploymentId: "d-1" } }],
        steps: [
          { id: "record-run", handler: () => undefined },
          { id: "plan-builds", handler: () => [] },
          { id: "placement-0", handler: () => ({ kind: "done" }) },
          { id: "refresh-mirrors", handler: () => undefined },
          { id: "run-deployment", handler: () => ({ ran: { id: "d-1", status: "applied", remove: false } }) },
          { id: "retire-old", handler: () => ({ ids: [] }) },
          { id: "sync-uptime", handler: () => undefined },
          { id: "forget-run", handler: () => undefined },
          { id: "delete-orphan-mirrors", handler: () => undefined },
        ],
        transformCtx: (ctx) => ({ ...mockCtx(ctx), runId: DEPLOY }),
      }).execute();

      expect((await deploy()).error).toBeUndefined();
      expect((await deploy()).error).toBeUndefined();

      const releases = (await lanes()).filter((lane) => lane.kind === "release");
      expect(releases).toMatchObject([{ state: "requested", phase: "committed" }]);
      const output = await execute(releases[0]?.id ?? "", "run-3");
      expect(output.result).toMatchObject({ moved: "fsn-2" });
      expect((await rows()).at(-1)).toMatchObject({ kind: "release", state: "done" });
      expect(await place()).toEqual([]);
      await finish();
      expect(await requested({ kind: "move", args: { to: "fsn-1" } })).toMatchObject({ kind: "move", state: "requested" });
    });

    it("on a committed lane whose B was renamed since the commit: it follows B by id and starts the app there", async () => {
      const cut = afterCut();
      await moving("committed");
      machines[1] = { id: idOf("fsn-2"), name: "fsn-two", pool: true, copy: slotCopy(cut) };

      const placed = await place();

      expect(placed).toMatchObject([{ kind: "follow", to: "fsn-2", machine: idOf("fsn-2"), epoch: 1 }]);
      expect(await fence(placed[0])).toEqual({ on: "fsn-two" });
      expect(verbs).toEqual(["writer@fsn-two(create)"]);
      expect(containers.filter((container) => container.running)).toEqual([{ machine: "fsn-two", id: `created-${volumeId}-${cut.guid}`, running: true }]);
    });

    it("on a quiet Volume: the Deploy holds it with no Floor call, a Move asked for meanwhile is refused, and the hold leaves no row", async () => {
      expect(await place()).toEqual([]);
      expect(await lanes()).toMatchObject([{ kind: "deploy", state: "running", inngest_run_id: DEPLOY, args: { deployment: "d-1", resume: null } }]);

      for (const end of [undefined, "yielded"] as const) {
        expect(await runEffect(requestVolumeRun({ userId, organizationId }, { volumeId, environment, kind: "move", args: { to: "fsn-2" } }, { end }))).toMatchObject({
          ok: false, refusal: { code: "conflict", message: "data is deploying; the Deploy ends its run" }, busy: expect.objectContaining({ kind: "deploy" }),
        });
      }
      expect(await place()).toEqual([]);
      expect(await lanes()).toHaveLength(1);
      expect(verbs).toEqual([]);
      expect(send).not.toHaveBeenCalled();

      expect(await finish()).toEqual({ ended: 0, requeued: [], refused: [] });
      expect(await lanes()).toEqual([]);
      expect(await requested({ kind: "move", args: { to: "fsn-2" } })).toMatchObject({ kind: "move", state: "requested" });
    });

    it("a Deploy that fails holding a quiet Volume frees it", async () => {
      await place();

      expect(await runEffect(finishDeployVolumes(DEPLOY, "failed"))).toEqual({ ended: 0, requeued: [], refused: [] });

      expect(await lanes()).toEqual([]);
    });

    it("a narrowed Deploy that does not touch the Volume leaves it free", async () => {
      deployNodes = [{ type: "service", id: "svc-api", name: "api", outcome: "pending", rows: [] }];

      expect(await place()).toEqual([]);

      expect(await lanes()).toEqual([]);
      expect(await requested({ kind: "move", args: { to: "fsn-2" } })).toMatchObject({ kind: "move" });
    });

    it("a second Deploy takes over the first's hold and holds the Volume until it finishes", async () => {
      await place();

      expect(await runEffect(placeDeployVolumes(organizationId, "env-1", "d-2", "deploy-2"))).toEqual([]);

      expect(await lanes()).toMatchObject([
        { kind: "deploy", state: "yielded", inngest_run_id: DEPLOY },
        { kind: "deploy", state: "running", inngest_run_id: "deploy-2", args: { deployment: "d-2", resume: null } },
      ]);
      expect(verbs).toEqual([]);
      expect(await finish()).toEqual({ ended: 0, requeued: [], refused: [] });
      expect(await request({ kind: "move", args: { to: "fsn-2" } })).toMatchObject({ ok: false, busy: expect.objectContaining({ kind: "deploy", args: expect.objectContaining({ deployment: "d-2" }) }) });
      expect(await runEffect(finishDeployVolumes("deploy-2", "done"))).toEqual({ ended: 0, requeued: [], refused: [] });
      expect(await lanes()).toEqual([]);
    });

    describe("a lane the Deploy places beside, which ends while the Deploy runs", () => {
      const deploying = { ok: false, refusal: { code: "conflict", message: "data is deploying; the Deploy ends its run" } } as const;
      const end = (id: string) => harness.pool.query("update volume_run set state = 'done', finished_at = now() where id = $1", [id]);
      const moves = async () => (await lanes()).filter((lane) => lane.kind === "move");

      it("a mirror: a Move, a release or a staged placement asked after it ends is refused, and leaves no row", async () => {
        const mirroring = await requested({ kind: "mirror", args: { to: "fsn-2" } });
        expect(await place()).toEqual([]);
        expect(await request({ kind: "move", args: { to: "fsn-2" } })).toMatchObject({ ...deploying, busy: expect.objectContaining({ id: mirroring.id, kind: "mirror" }) });

        await end(mirroring.id);
        send.mockClear();

        for (const end of [undefined, "yielded"] as const) {
          expect(await runEffect(requestVolumeRun({ userId, organizationId }, { volumeId, environment, kind: "move", args: { to: "fsn-2" } }, { end }))).toMatchObject({
            ...deploying, refusal: { ...deploying.refusal, details: { run: expect.objectContaining({ id: mirroring.id }) } }, busy: expect.objectContaining({ id: mirroring.id }),
          });
        }
        expect(await request({ kind: "release", args: {} })).toMatchObject(deploying);
        expect(await lanes()).toMatchObject([{ id: mirroring.id, kind: "mirror", state: "done" }]);
        expect(send).not.toHaveBeenCalled();

        expect(await finish()).toEqual({ ended: 0, requeued: [], refused: [] });
        expect(await requested({ kind: "move", args: { to: "fsn-2" } })).toMatchObject({ kind: "move", state: "requested" });
      });

      it("a committed Move the Deploy follows: a Move asked after it ends is refused until the Deploy finishes", async () => {
        afterCut();
        const committed = await moving("committed");
        expect(await place()).toMatchObject([{ kind: "follow", run: { id: committed.id } }]);

        await end(committed.id);

        expect(await request({ kind: "move", args: { to: "fsn-1" } })).toMatchObject({ ...deploying, busy: expect.objectContaining({ id: committed.id, kind: "move" }) });
        expect(await moves()).toMatchObject([{ id: committed.id, state: "done" }]);
        expect(send).not.toHaveBeenCalled();

        expect(await finish()).toEqual({ ended: 0, requeued: [], refused: [] });
        expect(await requested({ kind: "move", args: { to: "fsn-1" } })).toMatchObject({ kind: "move", state: "requested" });
      });

      it("another Deploy places over the stamp and takes it, so the Volume stays held until that Deploy finishes", async () => {
        const mirroring = await requested({ kind: "mirror", args: { to: "fsn-2" } });
        await place();

        expect(await runEffect(placeDeployVolumes(organizationId, "env-1", "d-2", "deploy-2"))).toEqual([]);
        expect(await finish()).toEqual({ ended: 0, requeued: [], refused: [] });
        await end(mirroring.id);
        expect(await request({ kind: "move", args: { to: "fsn-2" } })).toMatchObject({ ...deploying, busy: expect.objectContaining({ id: mirroring.id }) });

        expect(await runEffect(placeDeployVolumes(organizationId, "env-1", "d-3", "deploy-3"))).toEqual([]);
        expect(await lanes()).toMatchObject([{ id: mirroring.id, state: "done" }, { kind: "deploy", state: "running", inngest_run_id: "deploy-3", args: { hold: true } }]);
        expect(await runEffect(finishDeployVolumes("deploy-3", "done"))).toEqual({ ended: 0, requeued: [], refused: [] });
        expect(await request({ kind: "move", args: { to: "fsn-2" } })).toMatchObject({ ...deploying, busy: expect.objectContaining({ id: mirroring.id }) });

        expect(await runEffect(finishDeployVolumes("deploy-2", "done"))).toEqual({ ended: 0, requeued: [], refused: [] });
        expect(await requested({ kind: "move", args: { to: "fsn-2" } })).toMatchObject({ kind: "move", state: "requested" });
      });

      it("a Deploy's own stamp on the Volume refuses none of the moves it requests again", async () => {
        const taken = await moving();
        await place();
        await harness.pool.query("update volume_run set held_by = $2 where id = $1", [taken.id, DEPLOY]);

        expect(await finish()).toEqual({ ended: 1, requeued: [expect.any(String)], refused: [] });
        expect(await moves()).toMatchObject([{ id: taken.id, state: "yielded" }, { state: "requested", args: { to: "fsn-2" } }]);
      });
    });

    it("with a delete_mirror lane: plain beside it, no hold and no Floor call", async () => {
      const pruning = await requested({ kind: "delete_mirror", args: { slot: "fsn-2", confirmed_name: null, role: "any" } });

      expect(await place()).toEqual([]);

      expect(verbs).toEqual([]);
      expect(await lanes()).toMatchObject([{ id: pruning.id, kind: "delete_mirror", state: "requested" }]);
      expect(await finish()).toEqual({ ended: 0, requeued: [], refused: [] });
      expect(await lanes()).toMatchObject([{ id: pruning.id, kind: "delete_mirror", state: "requested" }]);
    });

    it("with a resize lane: plain beside it, stamped so a move asked for later is refused, and no size verb", async () => {
      storeVolumes = [{ id: volumeId, name: "data", storage: { kind: "provisioned", maximumBytes: 10 * GB }, recorded_storage: 10 * GB }];
      for (const fake of machines) fake.capabilities = [requirements.volumeFloor.capability, requirements.volumeSize.capability];
      const growing = await requested({ kind: "resize", args: { maximum_bytes: 20 * GB } });

      expect(await place()).toEqual([]);

      expect(verbs).toEqual([]);
      expect(await lanes()).toMatchObject([{ id: growing.id, kind: "resize", state: "requested" }]);
      expect((await harness.pool.query("select id, held_by from volume_run")).rows).toEqual([{ id: growing.id, held_by: DEPLOY }]);
      expect(await finish()).toEqual({ ended: 0, requeued: [], refused: [] });
      expect(await lanes()).toMatchObject([{ id: growing.id, kind: "resize", state: "requested" }]);
    });

    it("with a sync lane: the sync yields, since its demote cuts the writer, and the Deploy floors A with nothing to request again", async () => {
      const syncing = await requested({ kind: "sync", args: { full: false } });
      send.mockClear();

      const placed = await place();

      expect(placed).toMatchObject([{ kind: "took", run: { kind: "deploy" }, resume: null }]);
      expect(await lanes()).toMatchObject([{ id: syncing.id, state: "yielded", message: "a Deploy took this run over" }, { kind: "deploy", state: "running" }]);
      expect(await fence(placed[0])).toEqual({ on: "fsn-1" });
      expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2"]);
      expect(await finish()).toEqual({ ended: 1, requeued: [], refused: [] });
      expect(await lanes()).toMatchObject([{ id: syncing.id, state: "yielded" }, { kind: "deploy", state: "done" }]);
    });

    it("with a sync lane and two roots: the Deploy floors both and deploys on the newer-epoch root, the one the sync's demote keeps", async () => {
      machines = [{ name: "fsn-1", pool: true, copy: writerCopy(base, 1) }, { name: "fsn-2", pool: true, copy: writerCopy(base, 3) }];
      await requested({ kind: "sync", args: { full: false } });

      const [placed] = await place();

      expect(await fence(placed)).toEqual({ on: "fsn-2" });
      expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2"]);
      expect(machines.map((machine) => machine.copy)).toEqual([writerCopy(base, 1), writerCopy(base, 3)]);
    });

    it("with a sync lane and two roots at one epoch: the fence floors both, then fails naming both and promotes neither", async () => {
      machines = [{ name: "fsn-1", pool: true, copy: writerCopy(base, 2) }, { name: "fsn-2", pool: true, copy: writerCopy(base, 2) }];
      await requested({ kind: "sync", args: { full: false } });

      const [placed] = await place();

      await expect(fence(placed)).rejects.toThrow("data has two writers, data-fsn-1 and data-fsn-2");
      expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2"]);
      expect(machines.map((machine) => machine.copy)).toEqual([writerCopy(base, 2), writerCopy(base, 2)]);
    });

    it("the Deploy function floors, promotes, then runs the deployment, and ends its lane after", async () => {
      afterCut();
      const taken = await moving();

      const deployed = await new InngestTestEngine({
        function: createRunStoreDeployment(new Inngest({ id: "test" }), runEffect as Parameters<typeof createRunStoreDeployment>[1]),
        events: [{ name: "config/deployment.admitted", data: { organizationId, environmentId: "env-1", deploymentId: "d-1" } }],
        steps: [
          { id: "record-run", handler: () => undefined },
          { id: "plan-builds", handler: () => [] },
          { id: "placement-0", handler: () => ({ kind: "done" }) },
          { id: "refresh-mirrors", handler: () => undefined },
          { id: "run-deployment", handler: () => {
            verbs.push("run-deployment");
            return { ran: { id: "d-1", status: "applied", remove: false } };
          } },
          { id: "retire-old", handler: () => ({ ids: [] }) },
          { id: "sync-uptime", handler: () => undefined },
          { id: "forget-run", handler: () => undefined },
          { id: "delete-orphan-mirrors", handler: () => undefined },
        ],
        transformCtx: (ctx) => ({ ...mockCtx(ctx), runId: DEPLOY }),
      }).execute();

      expect(deployed.error).toBeUndefined();
      expect(verbs).toEqual(["floor@fsn-1", "floor@fsn-2", "writer@fsn-1(kept)", "run-deployment"]);
      expect(deployed.ctx.step.run).toHaveBeenCalledWith(`fence-${volumeId}`, expect.any(Function));
      expect(await lanes()).toMatchObject([
        { id: taken.id, kind: "move", state: "yielded" },
        { kind: "deploy", state: "done", inngest_run_id: DEPLOY },
        { kind: "move", state: "requested", args: { to: "fsn-2" } },
      ]);
    });

    it("a Deploy that fails after taking a move over ends its row failed and requests the move again, once", async () => {
      const taken = await moving();
      const [placed] = await place();
      await fence(placed);
      const worker = createRunStoreDeployment(new Inngest({ id: "test" }), runEffect);
      const failed = () => invokeOnFailure(worker, DEPLOY, {
        name: configDeploymentAdmittedEvent,
        data: { organizationId, environmentId: "env-1", deploymentId: "d-1" },
      });
      send.mockClear();

      await failed();

      expect(await lanes()).toMatchObject([
        { id: taken.id, state: "yielded" },
        { kind: "deploy", state: "failed", message: "the Deploy failed", inngest_run_id: DEPLOY },
        { kind: "move", state: "requested", args: { to: "fsn-2" } },
      ]);
      expect(send).toHaveBeenCalledTimes(1);
      expect(send).toHaveBeenCalledWith(expect.objectContaining({ name: "volume/run.requested" }));

      await failed();

      expect((await lanes()).filter((lane) => lane.kind === "move")).toHaveLength(2);
      expect(send).toHaveBeenCalledTimes(1);
    });

    it("a Deploy cancelled after taking a move over ends its row cancelled and requests the move again, once", async () => {
      const taken = await moving();
      const [placed] = await place();
      await fence(placed);
      const cancel = () => new InngestTestEngine({
        function: createCancelStoreDeployment(new Inngest({ id: "test" }), runEffect),
        events: [{ name: "inngest/function.cancelled", data: { function_id: "run-store-deployment", run_id: DEPLOY } }],
      }).execute();
      send.mockClear();

      expect((await cancel()).error).toBeUndefined();

      expect(await lanes()).toMatchObject([
        { id: taken.id, state: "yielded" },
        { kind: "deploy", state: "cancelled", message: "the Deploy was cancelled", inngest_run_id: DEPLOY },
        { kind: "move", state: "requested", args: { to: "fsn-2" } },
      ]);
      expect(send).toHaveBeenCalledTimes(1);

      expect((await cancel()).error).toBeUndefined();

      expect((await lanes()).filter((lane) => lane.kind === "move")).toHaveLength(2);
      expect(send).toHaveBeenCalledTimes(1);
    });

    it("a second Deploy taking over the first's running row carries the move it took, so the move is still requested again once", async () => {
      const taken = await moving();
      await place();

      const placed = await runEffect(placeDeployVolumes(organizationId, "env-1", "d-2", "deploy-2"));

      expect(placed).toMatchObject([{ kind: "took", resume: { to: "fsn-2" } }]);
      expect(await lanes()).toMatchObject([
        { id: taken.id, state: "yielded" },
        { kind: "deploy", state: "yielded", inngest_run_id: DEPLOY },
        { kind: "deploy", state: "running", inngest_run_id: "deploy-2", args: { deployment: "d-2", resume: { to: "fsn-2" } } },
      ]);
      expect(await finish()).toEqual({ ended: 0, requeued: [], refused: [] });
      expect(await runEffect(finishDeployVolumes("deploy-2", "done"))).toEqual({ ended: 1, requeued: [expect.any(String)], refused: [] });
      expect((await lanes()).filter((lane) => lane.kind === "move")).toMatchObject([{ id: taken.id }, { state: "requested", args: { to: "fsn-2" } }]);
    });

    describe("a lane landing while the Deploy places", () => {
      const landings = {
        sync: '{"full":false}',
        release: "{}",
        delete_mirror: '{"slot":"fsn-2","confirmed_name":null}',
      };
      const land = (landing: PoolClient, kind: string, args: string) => landing.query<{ id: string }>(
        `insert into volume_run (organization_id, environment_id, volume_id, volume_name, docker_volume, kind, args, state)
         values ($1, 'env-1', $2, 'data', $3, $4, $5, 'requested') returning id`,
        [organizationId, volumeId, dockerVolume, kind, args]);

      for (const [kind, args] of Object.entries(landings)) {
        it(`${kind} on a free Volume: the Deploy's hold waits for it and fails, and the retry places beside or over it`, async () => {
          const landing = await harness.pool.connect();
          let landed: { id: string } | undefined;
          try {
            await landing.query("begin");
            [landed] = (await land(landing, kind, args)).rows;

            const placing = place();
            await waitingOnLock();
            await landing.query("commit");

            await expect(placing).rejects.toThrow(`data is busy with a ${{ sync: "mirror refresh", release: "release", delete_mirror: "mirror removal" }[kind]}`);
          } finally {
            landing.release();
          }
          expect(await lanes()).toMatchObject([{ id: landed?.id, kind, state: "requested" }]);

          const placed = await place();

          if (kind === "delete_mirror") {
            expect(placed).toEqual([]);
            expect(await lanes()).toMatchObject([{ id: landed?.id, kind, state: "requested" }]);
          } else {
            expect(placed).toMatchObject([{ kind: "took", run: { kind: "deploy" }, resume: null }]);
            expect(await lanes()).toMatchObject([{ id: landed?.id, state: "yielded" }, { kind: "deploy", state: "running" }]);
          }
          expect(verbs).toEqual([]);
        });

        it(`${kind} during a warm move: the lane the Deploy locks is the only active row, so the landing cannot insert`, async () => {
          const taken = await moving();
          const landing = await harness.pool.connect();
          try {
            await landing.query("begin");
            await expect(land(landing, kind, args)).rejects.toMatchObject({ code: "23505" });
            await landing.query("rollback");
          } finally {
            landing.release();
          }

          const placed = await place();

          expect(placed).toMatchObject([{ kind: "took", run: { kind: "deploy" } }]);
          expect(await lanes()).toMatchObject([{ id: taken.id, state: "yielded" }, { kind: "deploy", state: "running" }]);
        });
      }
    });
  });
  describe("orphan hooks", () => {
    const deploy = (status: "applied" | "failed") => new InngestTestEngine({
      function: createRunStoreDeployment(new Inngest({ id: "test" }), runEffect as Parameters<typeof createRunStoreDeployment>[1]),
      events: [{ name: "config/deployment.admitted", data: { organizationId, environmentId: "env-1", deploymentId: "d-1" } }],
      steps: [
        { id: "record-run", handler: () => undefined },
        { id: "plan-builds", handler: () => [] },
        { id: "run-deployment", handler: () => ({ ran: { id: "d-1", status, remove: false } }) },
        { id: "retire-old", handler: () => ({ ids: [] }) },
        { id: "sync-uptime", handler: () => undefined },
        { id: "forget-run", handler: () => undefined },
      ],
    }).execute();

    it("an applied Deployment looks for orphan mirrors, a failed one does not", async () => {
      const applied = await deploy("applied");
      const failed = await deploy("failed");

      expect(applied.ctx.step.run).toHaveBeenCalledWith("delete-orphan-mirrors", expect.any(Function));
      expect(failed.ctx.step.run).not.toHaveBeenCalledWith("delete-orphan-mirrors", expect.any(Function));
    });

    it("the hourly sweep looks for orphan slots in every Organization with a cluster", async () => {
      const swept = await new InngestTestEngine({
        function: createCloseStaleVolumeRuns(new Inngest({ id: "test" }), runEffect),
        events: [{ name: "inngest/scheduled.timer", data: {} }],
        steps: [
          { id: "close-stale", handler: () => ({ lost: 0, closed: 0 }) },
          { id: "list-organizations", handler: () => [organizationId, "org-2"] },
          { id: "orphans-org-2", handler: () => ["r-1", "r-2"] },
        ],
      }).execute();

      expect(swept.ctx.step.run).toHaveBeenCalledWith(`orphans-${organizationId}`, expect.any(Function));
      expect(swept.result).toEqual({ lost: 0, closed: 0, orphanRuns: 2 });
    });
  });
});
