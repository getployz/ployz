import type {
  ConfigStore,
  ContainerId,
  CopyObservation,
  CopyRefusal,
  ExpectedCopy,
  MachineId,
  SetVolumeCopyReply,
  SetVolumeCopyRequest,
  Snapshot,
  VolumeCopy,
  VolumeCopyView,
  VolumeSwitchRequest,
  WantedCopy,
} from "@ployz/sdk";
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
import { PloyzProviderError, type PloyzSdkError, type PloyzSession } from "#/modules/runtime/ployz.server";
import { RuntimeConnectionFailure } from "#/modules/runtime/runtime-connection-errors";
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
  type RunContext,
  sendSwitch,
  setVolumeCopy,
  startOrphanDeletes,
  volumeBusyMessage,
} from "#/modules/volume-run/volume-run.server";
import { PATIENCE_WAITS } from "#/modules/volume-run/patience";
import type { Member, VolumeRunInput } from "#/modules/volume-run/volume-run";
import { member } from "#/modules/volume-run/volume-run.test-fixture";
import { type DeployLane, fenceDeployVolume, finishDeployVolumes, placeDeployVolumes } from "#/modules/volume-run/volume-run.deploy.server";
import type { Caller } from "#/modules/identity/actor";
import { runVolume } from "#/modules/operations/operations.server";
import { decideApproval } from "#/modules/approvals/approvals.server";
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
  /** The gen floor a Floor set raised; a set with a lower gen is stale. */
  floor?: number;
};

type FakeContainer = { machine: string; id: string; running: boolean; created?: number };

/**
 * What a fake Machine does with the next request of one label: refuse, apply it and lose the reply, fail to answer, or
 * hold it in flight until the test lands it.
 */
type Fault = CopyRefusal | "lost" | "down" | "held" | RawFault;

/** A Machine's error exactly as sent, for refusals whose copy differs from the fake's own. */
class RawFault {
  constructor(readonly cause: { code: string; message: string; details: unknown }) {}
}

const at = 1_790_000_000;
const snap = (guid: string, name = `dep-${guid}`): Snapshot => ({ name, guid, created_unix_seconds: at });
const base = snap("10");
const seedCopy: VolumeCopy = { kind: "root", readonly: false, newest: null, epoch: 0, seed: true };
const writerCopy = (newest: Snapshot | null = base, epoch = 0, readonly = false): VolumeCopy =>
  ({ kind: "root", readonly, newest, epoch, seed: false });
const slotCopy = (newest: Snapshot | null, pull: { target: string; failure: string | null } | null = null, slot = "1"): VolumeCopy =>
  ({ kind: "slot", slot, readonly: true, newest, resume_token: null, pull });
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
  /** Fails a runtime frame read, counted from 1 in `frames`, as a Server that never answers its first frame. */
  let frameFault: ((read: number) => boolean) | null;
  let frames: number;
  let sent: SetVolumeCopyRequest[];
  let held: Map<string, () => CopyRefusal | null>;
  let storeVolumes: Array<{ id: string; name: string; storage: { kind: "provisioned"; maximumBytes: number } | { kind: "docker" }; change?: "delete" }>;
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
        return copy?.kind === "slot" && copy.pull === null && copy.newest?.name === want.target;
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
        return copy?.kind === "slot" && copy.newest?.guid === expect.newest && copy.slot === expect.slot;
    }
  };

  /**
   * SetVolumeCopy as the daemon answers it: at the wanted state is success, then a landed pull of the wanted target
   * finishes whatever the expect, else the expect, else the transition. A receive lands on an inspect or a Mirror set and
   * keeps `pull`; a resend for the running target changes nothing.
   */
  const transition = (fake: FakeMachine, expect: ExpectedCopy, want: WantedCopy): CopyRefusal | null => {
    if (want.kind === "floor" || atWant(fake, want)) return null;
    const copy = fake.copy;
    if (want.kind === "mirror" && copy?.kind === "slot" && copy.pull?.target === want.target && copy.newest?.name === want.target) {
      fake.copy = { ...copy, pull: null };
      return null;
    }
    if (!matches(fake.copy, expect)) return "mismatch";
    if (copy?.kind === "root") {
      switch (want.kind) {
        case "mirror":
          for (const container of containers) if (container.machine === fake.name) container.running = false;
          fake.copy = slotCopy(newSnap(want.target), null, newSlot());
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
    watchFirstFrame: () => frameFault?.(++frames) === true
      ? Effect.fail(new RuntimeConnectionFailure({ cause: new Error("Runtime watch timed out") }))
      : Effect.succeed({
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
        created_at_unix_nanos: container.created ?? 0,
      })),
      incomplete_ids: { machines: [] },
    }),
    inspectContainer: (target: string, container: string) => Effect.promise(async () => {
      await beforeStart?.();
      return {
        container: {
          container_id: container,
          resolved_spec: { name: "web", container: { pull_policy: "always", environment: { POSTGRES_PASSWORD: `secret-${machine(target).name}` } }, volumes: [{ source: { kind: "provisioned", name: dockerVolume } }] },
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
    volumeSwitch: (target: string, request: VolumeSwitchRequest) => Effect.suspend((): Effect.Effect<VolumeCopyView | SetVolumeCopyReply, PloyzSdkError> => {
      const fake = machine(target);
      if (fake.silent === true) return Effect.never;
      if (fake.lacks !== undefined) return Effect.fail(new ServerLacks({ machineId: target, capability: requirements.volumeMove.capability }));
      const fail = (fault: Fault) => Effect.fail(new PloyzProviderError({ operation: "volume switch", cause: failure(fault, fake.copy) }));
      if (request.command === "inspect_volume_copy") {
        const fault = failNext.get(`inspect@${fake.name}`);
        if (fault !== undefined) {
          failNext.delete(`inspect@${fake.name}`);
          return fail(fault);
        }
        return Effect.succeed(inspect(fake));
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
          return fail("down");
        }
        if (fault === "held") {
          held.set(key, () => transition(fake, expected, want));
          return fail("down");
        }
        if (fault !== undefined) return fail(fault);
        const gen = request.payload.gen ?? 0;
        if (gen < (fake.floor ?? 0)) return fail("stale");
        if (want.kind === "floor") fake.floor = gen;
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
          read: async (_: string, query: { query: string; id?: string }) => query.query === "namespaces"
            ? { namespaces: [{ namespace: "shop-production", project: "shop", environment: "production" }] }
            : query.query === "deployment"
              ? { id: query.id, status: "running", namespace: "shop-production", environment: { id: "env-1", project: "shop", name: "production" }, nodes: deployNodes }
              : { environment: { id: "env-1", project: "shop", name: "production", revision: 1 }, volumes: storeVolumes },
        })),
      }),
      Effect.provideService(OrganizationRuntime, {
        cancel: () => Effect.void,
        open: () => Effect.succeed({ status: "connected" as const, connected: session }),
      }),
    ))) as typeof runInngestEffect;

  const request = (input: VolumeRunInput) =>
    runEffect(requestVolumeRun({ userId, organizationId }, { volumeId, environment, ...input }));
  const requested = async (input: VolumeRunInput) => {
    const result = await request(input);
    if (!result.ok) throw new Error(result.refusal.message);
    return result.run;
  };
  const waitingOnLock = async () => {
    for (let poll = 0; poll < 200; poll++) {
      const { rows: waiting } = await harness.pool.query(
        "select 1 from pg_stat_activity where datname = current_database() and wait_event_type = 'Lock'");
      if (waiting.length > 0) return;
      await new Promise((resolve) => setTimeout(resolve, 25));
    }
    throw new Error("nothing waited on the held row lock");
  };
  const settled = ["05-cut", "07-start", "03-start", "04-resume", "05-resume", "03-restore", "03-demote", "03-reset-forget-mirror", "03-reset-forget-writer", "04-forget"];
  const sleeps = [
    ...["03-r0", "03-r1", "03-r2", "06-final"].flatMap((prefix) => Array.from({ length: 9 }, (_, poll) => `${prefix}-backoff-${poll}`)),
    ...[...settled, ...["03-r0", "03-r1", "03-r2"].flatMap((round) => [`${round}-snapshot`, `${round}-pull`]), "06-final-pull",
      ...["fsn-2", "fsn-3"].flatMap((server) => [`04-forget-${server}`, `03-forget-${server}`])]
      .flatMap((id) => Array.from({ length: 6 }, (_, check) => `${id}-wait-${check + 1}`)),
    ...Array.from({ length: 8 }, (_, check) => `01-wait-${check}`),
  ].map((id) => ({ id, handler: () => undefined }));
  const execute = (runRowId: string, runId = "run-1", steps?: Array<{ id: string; handler: () => object | undefined }>, volume = volumeId, took?: string) => new InngestTestEngine({
    function: createRunVolume(new Inngest({ id: "test" }), runEffect),
    events: [{ name: "volume/run.requested", data: { organizationId, environment: "env-1", volumeId: volume, runId: runRowId, took } }],
    transformCtx: (ctx) => ({ ...mockCtx(ctx), runId }),
    steps: [...(steps ?? []), ...sleeps],
  }).execute();
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
    id, organizationId, volumeId, volumeName: "data", dockerVolume, kind: "sync", args: { full: false }, orphan: false, refquotaBytes: 0, gen: 0, phase: "warm", current: null,
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
    sent = [];
    held = new Map();
    guids = 0;
    slotIds = 0;
    snapshots = new Map([[base.name, base]]);
    containers = [{ machine: "fsn-1", id: "web-fsn-1", running: true }];
    storeVolumes = [{ id: volumeId, name: "data", storage: { kind: "provisioned", maximumBytes: 5_000_000 } }];
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
    it("volume_run_second_request_refused: VolumeBusy names the Volume and how to stop its run, until it ends", async () => {
      const first = await requested({ kind: "mirror", args: { to: "fsn-2" } });

      const second = await request({ kind: "sync", args: { full: false } });

      expect(second).toEqual({
        ok: false,
        refusal: {
          code: "conflict",
          message: "VolumeBusy: data is busy with a mirror; ployz volume release data stops it",
          details: { run: expect.objectContaining({ id: first.id }) },
        },
        busy: expect.objectContaining({ id: first.id, kind: "mirror", state: "requested" }),
      });
      expect(volumeBusyMessage({ kind: "sync", volumeName: "data" })).toBe("VolumeBusy: data is busy with a mirror refresh; ployz volume release data stops it");
      expect(await rows()).toHaveLength(1);

      await execute(first.id);
      expect((await request({ kind: "sync", args: { full: false } })).ok).toBe(true);
    });

    it("a mirror removal under way says it finishes on its own, and a release leaves it to, since it cannot retract a destroy a Machine started", async () => {
      const removal = await requested({ kind: "delete_mirror", args: { slot: "fsn-2", confirmed_name: null } });

      const removing = "data is busy with a mirror removal; it finishes on its own";
      expect(await request({ kind: "sync", args: { full: false } })).toMatchObject({ ok: false, refusal: { code: "conflict", message: `VolumeBusy: ${removing}` } });
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
        refusal: { code: "conflict", message: "VolumeBusy: data is starting on fsn-2; it finishes there", details: { run: expect.objectContaining({ id: committed.id }) } },
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
        ok: false, refusal: { message: "VolumeBusy: data is being copied into data-copy; it finishes on its own" },
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
      expect(await rows()).toEqual([expect.objectContaining({ kind: "move", state: "running", inngest_run_id: "run-taker", message: null, finished_at: null })]);
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
      const volume = { environmentId: "env-1", volumeName: "data", dockerVolume, refquotaBytes: 5_000_000 };
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

      it("refuses a move, a mirror, any sync, a seed and a removal of B's mirror or of every mirror, returning the release that failed as busy, and admits removing another mirror", async () => {
        const release = await pending();
        const before = (await rows()).length;
        const seed: VolumeRunInput = { kind: "seed", args: { into: { volumeId: "vol2", dockerVolume: "shop-production_vol-vol2", name: "data-copy" }, guid: G.guid, name: G.name } };

        for (const input of [
          { kind: "move", args: { to: "fsn-1" } },
          { kind: "mirror", args: { to: "fsn-3" } },
          { kind: "sync", args: { full: true } },
          { kind: "sync", args: { full: false } },
          { kind: "delete_mirror", args: { slot: "fsn-2", confirmed_name: null } },
          { kind: "delete_mirror", args: { slot: null, confirmed_name: null } },
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
        expect(await insert({ kind: "delete_mirror", args: { slot: "fsn-3", confirmed_name: null } })).toMatchObject({ ok: true });
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

    it("mirror: a pull that never lands gives up after six hours of checks backing off to five minutes", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: null, pullFault: { target: "dep-10", kind: "stall" } };
      const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });
      const waits = PATIENCE_WAITS.map((_, check) => ({ id: `03-r0-backoff-${check}`, handler: () => undefined }));

      await execute(run.id, "run-1", waits);

      expect(verbs.filter((verb) => verb === "pull@fsn-2(dep-10)")).toHaveLength(PATIENCE_WAITS.length + 1);
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
      const run = await requested({ kind: "delete_mirror", args: { slot: "fsn-2", confirmed_name: null } });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, destroyed: ["fsn-2"] });
      expect(verbs).toEqual(["forget@fsn-2", "forget@fsn-1"]);
      expect(machines[1]?.copy).toBeNull();
      expect(await rows()).toMatchObject([{ state: "done", epoch: null }]);
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

      expect(lost.error).toMatchObject({ name: "Error", message: "set_volume_copy on fsn-1: internal: dataset is busy" });
      expect(await rows()).toMatchObject([{ state: "running" }]);
      expect(named("fsn-1").copy).toMatchObject({ kind: "slot", readonly: true });
      verbs = [];

      const retried = await execute(run.id);

      expect(retried.result).toEqual({ runId: run.id, moved: "fsn-2" });
      expect(verbs).toEqual(["pull@fsn-2(f-1)", "pull@fsn-2(f-1)", "writer@fsn-2(create)", "remove@fsn-1(web-fsn-1)"]);
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
        expect(await rows()).toMatchObject([{ state: "failed", message: "Volume mirror needs a newer Ployz on fsn-2. 0.2.5 is out." }]);
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
        const run = await requested({ kind: "delete_mirror", args: { slot: "fsn-2", confirmed_name: null } });

        await execute(run.id);

        expect(verbs).toEqual([]);
        expect(await rows()).toMatchObject([{ state: "failed", message: "Mirror removal needs a newer Ployz on fsn-2. 0.2.5 is out." }]);
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

  describe("admission", () => {
    const caller: Caller = { userId, organization: { id: organizationId, slug: "acme" }, credential: { kind: "session", id: "session-1" }, role: "owner", support: null, via: "agent" };
    const run = (input: VolumeRunInput) => runEffect(runVolume(caller, { volumeId, environment, ...input }, { key: null, approvalId: null }));

    it("a Mirror Remove of the last copy asks first, names the copy it deletes, and writes no run", async () => {
      machines = [
        { name: "fsn-1", pool: true, copy: null },
        { name: "fsn-2", pool: true, copy: slotCopy(base) },
      ];

      const admitted = await run({ kind: "delete_mirror", args: { slot: null, confirmed_name: "data" } });

      expect(admitted).toMatchObject({
        ok: false,
        refusal: { code: "approval_required", details: { effects: [{ kind: "deletes_volume", node: `${idOf("fsn-2")}/${dockerVolume}` }] } },
      });
      expect(await rows()).toEqual([]);
      expect(send).not.toHaveBeenCalled();
    });

    it("a Mirror Remove on a busy Volume is refused as busy before it asks", async () => {
      machines = [
        { name: "fsn-1", pool: true, copy: null },
        { name: "fsn-2", pool: true, copy: slotCopy(base) },
      ];
      const busy = await requested({ kind: "sync", args: { full: false } });
      send.mockClear();

      const admitted = await run({ kind: "delete_mirror", args: { slot: null, confirmed_name: "data" } });

      expect(admitted).toEqual({
        ok: false,
        refusal: { code: "conflict", message: "VolumeBusy: data is busy with a mirror refresh; ployz volume release data stops it", details: { run: expect.objectContaining({ id: busy.id }) } },
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

      expect(admitted).toMatchObject({ ok: false, refusal: { message: "Volume move needs a newer Ployz on fsn-2." } });
      expect(await rows()).toEqual([]);
      expect(send).not.toHaveBeenCalled();
    });

    it("a Move off a writer without the generation floor is refused before any row, naming the writer", async () => {
      first(machines).capabilities = [requirements.volumeMove.capability];

      const admitted = await run({ kind: "move", args: { to: "fsn-2" } });

      expect(admitted).toMatchObject({ ok: false, refusal: { message: "Volume move needs a newer Ployz on fsn-1." } });
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
        await runEffect(decideApproval(caller, approval_id, { approve: { digest: approval } }));
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

    it("on a committed Move whose run failed: it follows the commitment to B without waiting, and refuses to start B once a later run settled the Volume", async () => {
      const cut = afterCut();
      const taken = await moving("committed");
      await harness.pool.query("update volume_run set state = 'failed', finished_at = now() where id = $1", [taken.id]);

      const placed = await place();

      expect(placed).toMatchObject([{ kind: "follow", run: { id: taken.id, kind: "move" }, to: "fsn-2", machine: idOf("fsn-2"), epoch: 1 }]);
      const restored = await requested({ kind: "restore", args: { from: "fsn-1" } });
      await harness.pool.query("update volume_run set state = 'failed', epoch = 2, finished_at = now() where id = $1", [restored.id]);
      await expect(fence(placed[0])).rejects.toThrow("data changed since this Deploy placed it; Deploy again");
      expect(verbs).toEqual([]);

      await harness.pool.query("delete from volume_run where id = $1", [restored.id]);
      expect(await fence(placed[0])).toEqual({ on: "fsn-2" });
      expect(verbs).toEqual(["writer@fsn-2(create)"]);
      expect(containers.filter((container) => container.running)).toEqual([{ machine: "fsn-2", id: `created-${volumeId}-${cut.guid}`, running: true }]);
    });

    describe("on a committed Move whose run failed", () => {
      const failedCommit = async () => {
        afterCut();
        const taken = await moving("committed");
        await harness.pool.query("update volume_run set state = 'failed', finished_at = now() where id = $1", [taken.id]);
      };

      it("removing B ends the commitment: the Volume is no longer owed and a Deploy follows nothing", async () => {
        await failedCommit();

        await runEffect(releaseLanesOff(organizationId, { id: idOf("fsn-2"), name: "fsn-2" }, []));
        machines = machines.filter((fake) => fake.name !== "fsn-2");

        expect(await runEffect(isVolumeOwed(organizationId, volumeId))).toBe(false);
        expect(await place()).toEqual([]);
        expect(await requested({ kind: "move", args: { to: "fsn-3" } })).toMatchObject({ kind: "move", state: "requested" });
      });

      it("a mirror removal running on another Server does not stop a Deploy from following it", async () => {
        await failedCommit();
        await requested({ kind: "delete_mirror", args: { slot: "fsn-3", confirmed_name: null } });

        expect(await place()).toMatchObject([{ kind: "follow", to: "fsn-2", ended: true }]);
      });

      it("a release requested after the Deploy placed it inherits the same commitment, and the follow still starts B", async () => {
        await failedCommit();
        const placed = await place();
        await requested({ kind: "release", args: {} });

        expect(await fence(placed[0])).toEqual({ on: "fsn-2" });
        expect(verbs).toEqual(["writer@fsn-2(create)"]);
      });

      it("a silent B fails the follow with what to do", async () => {
        await failedCommit();
        const placed = await place();
        machines = machines.filter((fake) => fake.name !== "fsn-2");

        await expect(fence(placed[0])).rejects.toThrow(
          "data finishes on fsn-2, which did not answer. Bring fsn-2 back or remove it with ployz server rm fsn-2. Restore from fsn-1's copy: ployz volume restore data --from fsn-1");
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
        containers = containers.filter((container) => container.machine !== "fsn-1");
        for (const container of containers) container.running = false;
        verbs = [];
      };

      it("a Deploy follows it again, keeps B the writer and starts nothing", async () => {
        await followedThenStopped();

        const placed = await place();

        expect(placed).toMatchObject([{ kind: "follow", to: "fsn-2" }]);
        expect(await fence(placed[0])).toEqual({ on: "fsn-2" });
        expect(verbs).toEqual(["writer@fsn-2(none)"]);
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
      const pruning = await requested({ kind: "delete_mirror", args: { slot: "fsn-2", confirmed_name: null } });

      expect(await place()).toEqual([]);

      expect(verbs).toEqual([]);
      expect(await lanes()).toMatchObject([{ id: pruning.id, kind: "delete_mirror", state: "requested" }]);
      expect(await finish()).toEqual({ ended: 0, requeued: [], refused: [] });
      expect(await lanes()).toMatchObject([{ id: pruning.id, kind: "delete_mirror", state: "requested" }]);
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
