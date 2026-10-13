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
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { InngestClient } from "#/modules/inngest/client";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { requirements, ServerLacks } from "#/modules/runtime/daemon-compat";
import { PloyzProviderError, type PloyzSdkError, type PloyzSession } from "#/modules/runtime/ployz.server";
import { createRunStoreDeployment } from "#/modules/config-store/store-deployment.inngest";
import type { InngestRunStatus } from "#/modules/inngest/client";
import { createCloseFinishedVolumeRun, createCloseStaleVolumeRuns, createRunVolume } from "#/modules/volume-run/volume-run.inngest";
import {
  closeStaleVolumeRuns,
  closeVolumeRun,
  orphanSlotNames,
  requestVolumeRun,
  requireOwner,
  type RunContext,
  sendSwitch,
  setVolumeCopy,
  startOrphanDeletes,
  volumeBusyMessage,
} from "#/modules/volume-run/volume-run.server";
import type { VolumeRunInput } from "#/modules/volume-run/volume-run";
import { member } from "#/modules/volume-run/volume-run.test-fixture";
import type { Caller } from "#/modules/identity/actor";
import { runVolume } from "#/modules/operations/operations.server";
import type { Database } from "#/server/database.server";
import { makeInngestEffectRunner, type runInngestEffect } from "#/server/run.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const organizationId = "00000000-0000-4000-8000-000000000e01";
const userId = "00000000-0000-4000-8000-000000000e02";
const volumeId = "vol1";
const dockerVolume = `shop-production_vol-${volumeId}`;
const environment = { project: "shop", environment: "production" };

const inngest = new Inngest({ id: "volume-run-test" });
const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

/** A pull to `target` that fails with a message, or whose Machine restarts mid-receive and keeps `pull` set. */
type PullFault = { target: string; kind: "fail" | "restart" };

type FakeMachine = {
  name: string;
  pool: boolean;
  stateless?: boolean;
  silent?: boolean;
  lacks?: string;
  copy: VolumeCopy | null;
  /** Writes since the writer's newest snapshot: a Snapshot takes a new one only while this is above zero. */
  writes?: number;
  pullFault?: PullFault;
  /** A restart stopped the running receive; the next Mirror set resumes it. */
  stopped?: boolean;
};

type FakeContainer = { machine: string; id: string; running: boolean };

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
const snap = (guid: string, name = `ployz-${guid}`): Snapshot => ({ name, guid, created_unix_seconds: at });
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
  let beforeSet: Map<string, () => void>;
  let sent: SetVolumeCopyRequest[];
  let held: Map<string, () => CopyRefusal | null>;
  let storeVolumes: Array<{ id: string; name: string; storage: { kind: "provisioned"; maximumBytes: number } | { kind: "docker" }; change?: "delete" }>;

  const machine = (target: string) => {
    const found = machines.find((entry) => idOf(entry.name) === target);
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
    if (atWant(fake, want)) return null;
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
    watchFirstFrame: () => Effect.succeed({
      machines: machines.map((fake) => ({
        machine: { id: idOf(fake.name), name: fake.name, public_key: Array.from({ length: 32 }, (_, index) => index), runtime: { daemon_version: fake.lacks ?? "" } },
        storage: fake.stateless === true ? { state: "stateless" } : fake.pool ? { state: "pool", size_bytes: 1, used_bytes: 0, free_bytes: 1 } : { state: "ready" },
      })),
      containers: containers.map((container) => ({
        machine_id: idOf(container.machine),
        kind: "service_container",
        container_id: container.id,
        namespace: "shop-production",
        resolved_spec: { name: "web", container: { pull_policy: "always", environment: {} }, volumes: [{ source: { kind: "provisioned", name: dockerVolume } }] },
        runtime: { state: container.running ? "running" : "exited" },
      })),
    }),
    inspectContainer: (target: string, container: string) => Effect.sync(() => ({
      container: {
        container_id: container,
        resolved_spec: { name: "web", container: { pull_policy: "always", environment: { POSTGRES_PASSWORD: `secret-${machine(target).name}` } }, volumes: [{ source: { kind: "provisioned", name: dockerVolume } }] },
      },
    })),
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
      beforeSet.get(key)?.();
      beforeSet.delete(key);
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
    }),
  });

  const runEffect = makeInngestEffectRunner(<A, E>(operation: Effect.Effect<A, E, Database | OrganizationRuntime | InngestClient | CloudStore>) =>
    harness.runEffect(operation.pipe(
      Effect.provideService(InngestClient, inngest),
      Effect.provideService(CloudStore, {
        open: Effect.succeed(asTestDouble<ConfigStore>()({
          read: async (_: string, query: { query: string }) => query.query === "namespaces"
            ? { namespaces: [{ namespace: "shop-production", project: "shop", environment: "production" }] }
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
  const settled = ["05-cut", "08-start", "13-start", "13-undo", "14-undo", "04-resume", "03-restore", "03-demote", "03-reset-forget-mirror", "03-reset-forget-writer", "04-forget"];
  const sleeps = [
    ...["03-r0", "03-r1", "03-r2", "06-final"].flatMap((prefix) => Array.from({ length: 9 }, (_, poll) => `${prefix}-wait-${poll}`)),
    ...[...settled, ...["03-r0", "03-r1", "03-r2"].flatMap((round) => [`${round}-snapshot`, `${round}-pull`]), "06-final-pull",
      ...["fsn-2", "fsn-3"].flatMap((server) => [`13-forget-${server}`, `03-forget-${server}`])]
      .flatMap((id) => Array.from({ length: 6 }, (_, check) => `${id}-wait-${check + 1}`)),
  ].map((id) => ({ id, handler: () => undefined }));
  const execute = (runRowId: string, runId = "run-1", steps?: Array<{ id: string; handler: () => object | undefined }>, volume = volumeId) => new InngestTestEngine({
    function: createRunVolume(new Inngest({ id: "test" }), runEffect),
    events: [{ name: "volume/run.requested", data: { organizationId, environment: "env-1", volumeId: volume, runId: runRowId } }],
    transformCtx: (ctx) => ({ ...mockCtx(ctx), runId }),
    steps: [...(steps ?? []), ...sleeps],
  }).execute();
  const rows = async () => (await harness.pool.query(
    `select id, kind, state, epoch, inngest_run_id, message, orphan, volume_id, docker_volume, refquota_bytes, finished_at
     from volume_run order by created_at, id`,
  )).rows as Array<{
    id: string; kind: string; state: string; epoch: string | null; inngest_run_id: string | null; message: string | null;
    orphan: boolean; volume_id: string; docker_volume: string; refquota_bytes: string; finished_at: Date | null;
  }>;
  const context = (id: string): RunContext => ({
    id, organizationId, volumeId, volumeName: "data", dockerVolume, kind: "sync", args: { full: false }, orphan: false, refquotaBytes: 0,
  });
  const running = async (id: string) =>
    harness.pool.query(`update volume_run set state = 'running', inngest_run_id = 'run-1' where id = $1`, [id]);

  beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
  afterAll(async () => { await harness?.stop(); });

  beforeEach(async () => {
    verbs = [];
    failNext = new Map();
    beforeSet = new Map();
    sent = [];
    held = new Map();
    guids = 0;
    slotIds = 0;
    snapshots = new Map([[base.name, base]]);
    containers = [{ machine: "fsn-1", id: "web-fsn-1", running: true }];
    storeVolumes = [{ id: volumeId, name: "data", storage: { kind: "provisioned", maximumBytes: 5_000_000 } }];
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

      expect(swept).toEqual({ lost: 1, closed: 4 });
      expect(looked.sort()).toEqual(["r-cancelled", "r-completed", "r-failed", "r-missing", "r-running"]);
      expect(Object.fromEntries((await rows()).map((row) => [row.id.slice(-2), row.state]))).toEqual({
        "01": "lost", "02": "requested", "03": "lost", "04": "failed", "05": "cancelled", "06": "lost", "07": "running", "08": "running",
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
    it("volume_run_second_request_refused: VolumeBusy names the run in the way, until it ends", async () => {
      const first = await requested({ kind: "mirror", args: { to: "fsn-2" } });

      const second = await request({ kind: "sync", args: { full: false } });

      expect(second).toEqual({
        ok: false,
        refusal: {
          code: "conflict",
          message: `VolumeBusy: run ${first.id} (mirror) is requested; volume runs ${first.id} shows it`,
          details: { run: expect.objectContaining({ id: first.id }) },
        },
      });
      expect(volumeBusyMessage({ id: first.id, kind: "mirror", state: "running" }))
        .toBe(`VolumeBusy: run ${first.id} (mirror) is running; volume runs ${first.id} shows it`);
      expect(await rows()).toHaveLength(1);

      await execute(first.id);
      expect((await request({ kind: "sync", args: { full: false } })).ok).toBe(true);
    });
  });


  describe("epoch", () => {
    it("takes one above the highest epoch a copy shows, in its root or its newest snapshot's name", async () => {
      machines = [
        { name: "fsn-1", pool: true, copy: writerCopy(snap("10", "w-7-2"), 3) },
        { name: "fsn-2", pool: true, copy: slotCopy(snap("10", "f-5")) },
        { name: "fsn-3", pool: false, copy: null },
        { name: "docker-1", pool: false, stateless: true, copy: null, silent: true },
      ];
      const run = await requested({ kind: "sync", args: { full: false } });

      const output = await execute(run.id);

      expect(output.error).toBeUndefined();
      expect(await rows()).toMatchObject([{ epoch: "8", state: "done" }]);
      expect(verbs).toEqual(["snapshot@fsn-1(w-8-0)"]);
    });

    it("takes one above every epoch this Volume's runs took, so a Machine reset can't reuse one", async () => {
      await harness.pool.query(
        `insert into volume_run (organization_id, environment_id, volume_id, volume_name, docker_volume, kind, args, state, epoch, finished_at)
         values ($1, 'env-1', $2, 'data', $3, 'sync', '{"full":false}', 'done', 12, now())`,
        [organizationId, volumeId, dockerVolume]);
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(base) };
      const run = await requested({ kind: "sync", args: { full: false } });

      await execute(run.id);

      expect((await rows()).find((row) => row.id === run.id)).toMatchObject({ epoch: "13", state: "done" });
    });

    it("a replay reads back the epoch the run took", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(base) };
      const run = await requested({ kind: "sync", args: { full: false } });
      await harness.pool.query(`update volume_run set epoch = 40 where id = $1`, [run.id]);

      await execute(run.id);

      expect(await rows()).toMatchObject([{ epoch: "40", state: "done" }]);
      expect(verbs).toEqual(["snapshot@fsn-1(w-40-0)"]);
    });
  });

  describe("mirror and sync", () => {
    it("mirror: pulls the writer's newest, and stops once a round finds nothing new", async () => {
      const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, rounds: 1 });
      expect(verbs).toEqual(["snapshot@fsn-1(w-1-0)", "pull@fsn-2(ployz-10)", "pull@fsn-2(ployz-10)", "snapshot@fsn-1(w-1-1)"]);
      expect(sent[1]).toEqual({ name: dockerVolume, expect: { kind: "none" }, want: { kind: "mirror", from: expect.any(String), target: "ployz-10", drain: 3_000 } });
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
      machines[1] = { name: "fsn-2", pool: true, copy: null, pullFault: { target: "ployz-10", kind: "fail" } };
      const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });

      await execute(run.id);

      expect(await rows()).toMatchObject([{ state: "failed", message: "data-fsn-2 could not receive ployz-10: disk full" }]);
    });

    it("mirror: a Server without a Pool gets the mirror", async () => {
      machines[1] = { name: "fsn-2", pool: false, copy: null };
      const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, rounds: 1 });
      expect(verbs).toEqual(["snapshot@fsn-1(w-1-0)", "pull@fsn-2(ployz-10)", "pull@fsn-2(ployz-10)", "snapshot@fsn-1(w-1-1)"]);
      expect(await rows()).toMatchObject([{ state: "done", message: null }]);
    });

    it("sync: a mirror already at the writer's newest needs no pull", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(base) };
      const run = await requested({ kind: "sync", args: { full: false } });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, rounds: 0 });
      expect(verbs).toEqual(["snapshot@fsn-1(w-1-0)"]);
    });

    it("sync --full: forgets both copies' shared snapshots, then pulls from nothing", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(base) };
      const run = await requested({ kind: "sync", args: { full: true } });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, rounds: 1 });
      expect(verbs).toEqual(["forget@fsn-2", "forget@fsn-1", "snapshot@fsn-1(w-1-0)", "pull@fsn-2(ployz-10)", "pull@fsn-2(ployz-10)", "snapshot@fsn-1(w-1-1)"]);
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

    it("a cut refused as written undoes to A, which starts its kept Container", async () => {
      failNext.set("cut@fsn-1", "written");
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.error).toBeDefined();
      expect(verbs.slice(-2)).toEqual(["cut@fsn-1(f-1)", "writer@fsn-1(kept)"]);
      expect(sent.at(-1)).toMatchObject({ expect: { kind: "writer", epoch: 0 }, want: { kind: "writer", epoch: 1 } });
      expect(named("fsn-1").copy).toMatchObject({ kind: "root", readonly: false, epoch: 1 });
      expect(containers).toEqual([{ machine: "fsn-1", id: "web-fsn-1", running: true }]);
      expect(await rows()).toMatchObject([{ state: "failed", message: "data on fsn-1 was written after its final snapshot" }]);
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

    it("a final pull that fails forgets nothing B lacks and makes A the writer again", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: null, pullFault: { target: "f-1", kind: "fail" } };
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.error).toBeDefined();
      expect(verbs.slice(-4)).toEqual(["cut@fsn-1(f-1)", "pull@fsn-2(f-1)", "pull@fsn-2(f-1)", "writer@fsn-1(kept)"]);
      expect(verbs.filter((verb) => verb === "pull@fsn-2(f-1)")).toHaveLength(2);
      expect(named("fsn-1").copy).toMatchObject({ kind: "root", readonly: false, epoch: 1, newest: at("f-1") });
      expect(machines.filter((fake) => fake.copy?.kind === "root")).toHaveLength(1);
      expect(await rows()).toMatchObject([{ state: "failed", message: "data-fsn-2 could not receive f-1: disk full" }]);
    });

    it("a final pull a restart of B stopped resumes and lands", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: null, pullFault: { target: "f-1", kind: "restart" } };
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, moved: "fsn-2" });
      expect(verbs.filter((verb) => verb === "pull@fsn-2(f-1)")).toHaveLength(3);
      expect(named("fsn-2")).toMatchObject({ stopped: false, copy: { kind: "root", readonly: false, newest: at("f-1") } });
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
      ["a writer at another snapshot", () => writerCopy(base, 5), "data-fsn-2 writes at a snapshot other than f-1"],
      ["a read-only copy mid-cut", () => writerCopy(snapshots.get("f-1") ?? null, 5, true), "data-fsn-2 is a read-only copy mid-cut"],
    ])("an undo that finds B %s refuses and never makes A the writer", async (_, became, state) => {
      cut(newSnap("f-1"));
      beforeSet.set("pull@fsn-2", () => { named("fsn-2").copy = became(); });
      failNext.set("pull@fsn-2", "no_capacity");
      const run = await requested({ kind: "move", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.error).toBeDefined();
      expect(verbs).toEqual(["pull@fsn-2(f-1)"]);
      expect(named("fsn-1").copy).toMatchObject({ kind: "slot" });
      expect(containers).toEqual([{ machine: "fsn-1", id: "web-fsn-1", running: false }]);
      expect((await rows())[0]?.message).toContain(state);
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
      expect(verbs).toEqual(["writer@fsn-1(kept)"]);
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

    it("a cut root with no snapshot fails Release with the move to run again", async () => {
      named("fsn-1").copy = writerCopy(null, 0, true);
      for (const container of containers) container.running = false;
      const run = await requested({ kind: "release", args: {} });

      const output = await execute(run.id);

      expect(output.error).toBeDefined();
      expect(verbs).toEqual([]);
      expect(named("fsn-1").copy).toMatchObject({ kind: "root", readonly: true });
      expect(await rows()).toMatchObject([{ state: "failed", message: "data on fsn-1 stopped before its first snapshot; volume move data --to <another server> again" }]);
    });

    it("an idle writer is already where Release leaves it", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(base) };
      const run = await requested({ kind: "release", args: {} });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, released: "fsn-1" });
      expect(verbs).toEqual(["writer@fsn-1(kept)"]);
      expect(machines[1]?.copy).toEqual(slotCopy(base));
    });

    it("a writer whose Move reached B drops the Container A kept", async () => {
      const at = G();
      machines[1] = { name: "fsn-2", pool: true, copy: writerCopy(at, 1) };
      containers.push({ machine: "fsn-2", id: "created-1", running: true });
      const run = await requested({ kind: "release", args: {} });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, released: "fsn-2" });
      expect(verbs).toEqual(["writer@fsn-2(kept)", "remove@fsn-1(web-fsn-1)"]);
    });

    it("with only slots, forgets B's copy at G before A becomes the writer again", async () => {
      const at = G();
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(at) };
      const run = await requested({ kind: "release", args: {} });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, released: "fsn-1" });
      expect(verbs).toEqual(["forget@fsn-2", "writer@fsn-1(kept)"]);
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

      const late = await runEffect(setVolumeCopy({ ...context(move.id), kind: "move", args: { to: "fsn-2" } }, "run-1", { id: idOf("fsn-2"), name: "fsn-2" }, { kind: "mirror", newest: at.guid, slot: "1" }, {
        kind: "writer",
        at: at.guid,
        epoch: 1,
        start: { kind: "kept", container_id: "web-fsn-1" as ContainerId },
      }));

      expect(late).toMatchObject({ ok: false, reason: "mismatch", copy: null });
      expect(machines.filter((fake) => fake.copy?.kind === "root" && !fake.copy.readonly).map((fake) => fake.name)).toEqual(["fsn-1"]);
    });

    it("with only slots, a B that became the writer and runs the Service meanwhile sends the Move forward", async () => {
      const at = G();
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(at) };
      beforeSet.set("forget@fsn-2", () => {
        named("fsn-2").copy = writerCopy(at, 1);
        containers.push({ machine: "fsn-2", id: "created-1", running: true });
      });
      const run = await requested({ kind: "release", args: {} });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, released: null });
      expect(verbs).toEqual(["forget@fsn-2", "remove@fsn-1(web-fsn-1)"]);
    });

    it("with only slots, a B that became the writer but runs nothing gets B's start again before A's Container goes", async () => {
      const at = G();
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(at) };
      beforeSet.set("forget@fsn-2", () => { named("fsn-2").copy = writerCopy(at, 1); });
      const run = await requested({ kind: "release", args: {} });

      const output = await execute(run.id);

      expect(output.result).toEqual({ runId: run.id, released: null });
      expect(verbs).toEqual(["forget@fsn-2", "writer@fsn-2(create)", "remove@fsn-1(web-fsn-1)"]);
      expect(sent.at(-1)).toMatchObject({ expect: { kind: "writer", epoch: 1 }, want: { kind: "writer", at: at.guid, epoch: 1, start: { kind: "create", creation_key: `${volumeId}-${at.guid}` } } });
      expect(containers).toEqual([{ machine: "fsn-2", id: `created-${volumeId}-${at.guid}`, running: true }]);
    });

    it("with only slots, a B start that fails keeps A's Container, so the Service is not down on both Machines", async () => {
      const at = G();
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(at) };
      beforeSet.set("forget@fsn-2", () => { named("fsn-2").copy = writerCopy(at, 1); });
      failNext.set("writer@fsn-2", "no_capacity");
      const run = await requested({ kind: "release", args: {} });

      const output = await execute(run.id);

      expect(output.error).toBeDefined();
      expect(verbs).toEqual(["forget@fsn-2", "writer@fsn-2(create)"]);
      expect(containers).toEqual([{ machine: "fsn-1", id: "web-fsn-1", running: false }]);
      expect(await rows()).toMatchObject([{ state: "failed" }]);
    });

    it("with only slots and no kept Container, says to restore", async () => {
      const at = G();
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(at) };
      containers = [];
      const run = await requested({ kind: "release", args: {} });

      await execute(run.id);

      expect(verbs).toEqual([]);
      expect(await rows()).toMatchObject([{ state: "failed", message: "data has no writer and no stopped Service on its copies; volume restore data --from <server>" }]);
    });
  });

  describe("late requests", () => {
    const writable = () => machines.filter((fake) => fake.copy?.kind === "root" && !fake.copy.readonly).map((fake) => fake.name);
    const land = (key: string) => {
      const request = held.get(key);
      if (request === undefined) throw new Error(`nothing held for ${key}`);
      held.delete(key);
      request();
      inspect(named("fsn-2"));
    };

    const endTwoRuns = async () => {
      failNext.set("writer@fsn-2", "held");
      const move = await requested({ kind: "move", args: { to: "fsn-2" } });
      await execute(move.id, "run-1");
      const G = snapshots.get("f-1");
      expect(sent.at(-1)).toMatchObject({ expect: { kind: "mirror", newest: G?.guid }, want: { kind: "writer", at: G?.guid } });
      await runEffect(closeVolumeRun("run-1", "cancellation"));

      const release = await requested({ kind: "release", args: {} });
      await execute(release.id, "run-2");
      expect(verbs.slice(-2)).toEqual(["forget@fsn-2", "writer@fsn-1(kept)"]);

      failNext.set("pull@fsn-2", "held");
      const mirror = await requested({ kind: "mirror", args: { to: "fsn-2" } });
      await execute(mirror.id, "run-3");
      expect(sent.at(-1)).toMatchObject({ expect: { kind: "none" }, want: { kind: "mirror", target: "f-1" } });
      await runEffect(closeVolumeRun("run-3", "cancellation"));
    };

    it("two ended runs whose requests land late, B's start then B's pull, leave one writable root", async () => {
      await endTwoRuns();

      land("writer@fsn-2");
      land("pull@fsn-2");

      expect(writable()).toEqual(["fsn-1"]);
    });

    it("two ended runs whose requests land late, B's pull then B's start, leave one writable root", async () => {
      await endTwoRuns();

      land("pull@fsn-2");
      land("writer@fsn-2");

      expect(named("fsn-2").copy).toMatchObject({ kind: "slot", newest: snapshots.get("f-1") });
      expect(writable()).toEqual(["fsn-1"]);
    });

    it("a cut from an ended run that lands after the undo before the cut refuses, and A keeps writing and running", async () => {
      failNext.set("cut@fsn-1", "held");
      const move = await requested({ kind: "move", args: { to: "fsn-2" } });
      await execute(move.id, "run-1");
      failNext.set("cut@fsn-1", "written");
      await execute(move.id, "run-1");
      expect(await rows()).toMatchObject([{ state: "failed", epoch: "1" }]);

      const refused = held.get("cut@fsn-1");
      held.delete("cut@fsn-1");

      expect(refused?.()).toBe("mismatch");
      expect(named("fsn-1").copy).toMatchObject({ kind: "root", readonly: false, epoch: 1 });
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
      expect(await rows()).toMatchObject([{ state: "failed", message: "data is mid-run; wait or volume release data" }]);
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
    const caller: Caller = { userId, organization: { id: organizationId, slug: "acme" }, credential: { kind: "session", id: "session-1" }, support: null };
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
