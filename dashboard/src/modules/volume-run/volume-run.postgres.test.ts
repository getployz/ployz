import type { ConfigStore, CopyObservation, LeaseRecord, MachineId, ReceiveView, Snapshot, SwitchReply, VolumeCopy, VolumeCopyView, VolumeSwitchRequest } from "@ployz/sdk";
import { InngestTestEngine, mockCtx } from "@inngest/test";
import { Effect, Exit } from "effect";
import { Inngest, NonRetriableError } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { InngestClient } from "#/modules/inngest/client";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { PloyzProviderError, type PloyzSession } from "#/modules/runtime/ployz.server";
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
  startOrphanDeletes,
  volumeBusyMessage,
} from "#/modules/volume-run/volume-run.server";
import type { VolumeRunInput } from "#/modules/volume-run/volume-run";
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

type FakeMachine = {
  name: string;
  pool: boolean;
  stateless?: boolean;
  silent?: boolean;
  copy: VolumeCopy | null;
  lease: LeaseRecord | null;
  warms?: string[];
  receiving?: { target: string; guid: string; polls: number; resumable?: boolean };
};

const snap = (guid: string): Snapshot => ({ name: `ployz-${guid}`, guid, created_unix_seconds: 1_790_000_000 });
const writerCopy = (guid: string | null): VolumeCopy =>
  ({ kind: "root", writer: { phase: "idle" }, readonly: false, newest: guid === null ? null : snap(guid) });
const slotCopy = (guid: string | null, phase: "idle" | "handed_in" = "idle"): VolumeCopy =>
  ({ kind: "slot", mirror: phase === "idle" ? { phase } : { phase, guid: "1" }, readonly: true, newest: guid === null ? null : snap(guid), resume_token: null });
const idOf = (name: string) => name.padEnd(32, "0") as MachineId;
const first = <T>(items: readonly T[]): T => {
  const item = items[0];
  if (item === undefined) throw new Error("expected at least one item");
  return item;
};

const ordered = (sent: readonly string[]) => {
  const keyOf = (verb: string) => verb.startsWith("adopt_lease@") ? "adopt_lease" : verb.replace(/@[^(]*/, "@");
  const groups: string[][] = [];
  for (const verb of sent) {
    const last = groups.at(-1);
    if (last !== undefined && keyOf(last[0] ?? "") === keyOf(verb)) last.push(verb);
    else groups.push([verb]);
  }
  return groups.flatMap((group) => group.sort());
};

describe("volume runs", () => {
  let harness: PostgresTestHarness;
  let machines: FakeMachine[];
  let verbs: string[];
  let failNext: Map<string, unknown>;
  let sent: VolumeSwitchRequest[];
  let storeVolumes: Array<{ id: string; name: string; storage: { kind: "provisioned"; maximumBytes: number } | { kind: "docker" }; change?: "delete" }>;

  const machine = (target: string) => {
    const found = machines.find((entry) => idOf(entry.name) === target);
    if (found === undefined) throw new Error(`no fake Machine ${target}`);
    return found;
  };

  function answer(fake: FakeMachine, request: VolumeSwitchRequest): VolumeCopyView | ReceiveView | SwitchReply {
    const reply = (): SwitchReply => ({ decision: "admit", lease: fake.lease ?? { lease: 0, pos: { seq: 0, round: 0, sub: 0 }, cycle: "closed" }, copy: fake.copy });
    switch (request.command) {
      case "inspect_volume_copy":
        return { copy: fake.copy, lease: fake.lease };
      case "inspect_receive": {
        const receiving = fake.receiving;
        if (receiving === undefined) return { round: null, status: { state: "idle" } };
        receiving.polls += 1;
        if (receiving.resumable === true) {
          receiving.resumable = false;
          return { round: request.payload.round, status: { state: "resumable", target: receiving.target, token: "tok-1" } };
        }
        if (receiving.polls < 2) return { round: request.payload.round, status: { state: "running", target: receiving.target } };
        fake.copy = slotCopy(receiving.guid);
        fake.receiving = undefined;
        return { round: request.payload.round, status: { state: "done", newest: snap(receiving.guid) } };
      }
      case "adopt_lease":
        fake.lease = { lease: request.payload.lease, pos: { seq: 2, round: 0, sub: 0 }, cycle: "closed" };
        return reply();
      case "declare_mirror":
        fake.copy = slotCopy(null);
        return reply();
      case "destroy_mirror":
        fake.copy = null;
        return reply();
      case "warm_snapshot": {
        const next = fake.warms?.length === 1 ? fake.warms[0] : fake.warms?.shift();
        if (next !== undefined) fake.copy = writerCopy(next);
        return reply();
      }
      case "start_receive": {
        const guid = request.payload.target.replace("ployz-", "");
        fake.receiving ??= { target: request.payload.target, guid, polls: 0 };
        return reply();
      }
      default:
        return reply();
    }
  }

  const session = asTestDouble<PloyzSession>()({
    watchFirstFrame: () => Effect.succeed({
      machines: machines.map((fake) => ({
        machine: { id: idOf(fake.name), name: fake.name, public_key: Array.from({ length: 32 }, (_, index) => index) },
        storage: fake.stateless === true ? { state: "stateless" } : fake.pool ? { state: "pool", size_bytes: 1, used_bytes: 0, free_bytes: 1 } : { state: "ready" },
      })),
    }),
    volumeSwitch: (target: string, request: VolumeSwitchRequest) => Effect.suspend(() => {
      const fake = machine(target);
      sent.push(request);
      if (fake.silent === true) return Effect.never;
      const payload = request.payload as { switch?: { pos: { seq: number; round: number; sub: number } } };
      const pos = payload.switch?.pos;
      if (request.command !== "inspect_volume_copy" && request.command !== "inspect_receive") {
        verbs.push(`${request.command}@${fake.name}${pos === undefined ? "" : `(${pos.seq},${pos.round},${pos.sub})`}`);
      }
      const failure = failNext.get(`${request.command}@${fake.name}`);
      if (failure !== undefined) {
        failNext.delete(`${request.command}@${fake.name}`);
        return Effect.fail(new PloyzProviderError({ operation: "volume switch", cause: failure }));
      }
      return Effect.succeed(answer(fake, request));
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
  const execute = (runRowId: string, runId = "run-1", steps?: Array<{ id: string; handler: () => object | undefined }>, volume = volumeId) => new InngestTestEngine({
    function: createRunVolume(new Inngest({ id: "test" }), runEffect),
    events: [{ name: "volume/run.requested", data: { organizationId, environment: "env-1", volumeId: volume, runId: runRowId } }],
    transformCtx: (ctx) => ({ ...mockCtx(ctx), runId }),
    steps: [...(steps ?? []), ...[0, 1, 2].flatMap((round) =>
      Array.from({ length: 6 }, (_, poll) => ({ id: `04-r${round}-3-wait-${poll}`, handler: () => undefined })))],
  }).execute();
  const rows = async () => (await harness.pool.query(
    `select id, kind, state, lease, inngest_run_id, message, orphan, volume_id, docker_volume, refquota_bytes, finished_at
     from volume_run order by created_at, id`,
  )).rows as Array<{
    id: string; kind: string; state: string; lease: string | null; inngest_run_id: string | null; message: string | null;
    orphan: boolean; volume_id: string; docker_volume: string; refquota_bytes: string; finished_at: Date | null;
  }>;
  const context = (id: string): RunContext => ({
    id, organizationId, volumeId, volumeName: "data", dockerVolume, kind: "sync", args: { full: false }, orphan: false, refquotaBytes: 0,
  });

  beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
  afterAll(async () => { await harness?.stop(); });

  beforeEach(async () => {
    verbs = [];
    failNext = new Map();
    sent = [];
    storeVolumes = [{ id: volumeId, name: "data", storage: { kind: "provisioned", maximumBytes: 5_000_000 } }];
    machines = [
      { name: "fsn-1", pool: true, copy: writerCopy("10"), lease: null, warms: ["21"] },
      { name: "fsn-2", pool: true, copy: null, lease: null },
    ];
    send.mockReset();
    send.mockResolvedValue({ ids: [] });
    await harness.pool.query(`
      truncate table organization, "user" cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
      insert into "user" (id, email, name) values ('${userId}', 'member@example.com', 'Member');
    `);
  });

  describe("C15", () => {
    it("volume_run_row_lifecycle: requested, sent as volume-run-<id>, claimed, then done", async () => {
      const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });

      expect(run).toMatchObject({ volume_id: volumeId, volume_name: "data", kind: "mirror", state: "requested", lease: null, orphan: false });
      expect(send).toHaveBeenCalledWith(expect.objectContaining({ id: `volume-run-${run.id}`, name: "volume/run.requested" }));
      expect(await rows()).toMatchObject([{ state: "requested", docker_volume: dockerVolume, refquota_bytes: "5000000", inngest_run_id: null }]);

      const output = await execute(run.id);

      expect(output.error).toBeUndefined();
      expect(await rows()).toMatchObject([{ state: "done", inngest_run_id: "run-1", lease: "1", message: null, finished_at: expect.any(Date) }]);
    });

    it("a request whose event can't be sent ends not_started, and frees the Volume", async () => {
      send.mockRejectedValueOnce(new Error("Inngest is down"));
      const run = await requested({ kind: "sync", args: { full: false } });

      expect(run.state).toBe("not_started");
      expect(await rows()).toMatchObject([{ state: "not_started", finished_at: expect.any(Date) }]);
      expect((await request({ kind: "sync", args: { full: false } })).ok).toBe(true);
    });

    it("rerun_from_step_refused: a rerun under a new run id stops at its first effect step", async () => {
      const run = await requested({ kind: "sync", args: { full: false } });
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy("10"), lease: null };
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
      expect(verbs.slice(0, 2).sort()).toEqual(["adopt_lease@fsn-1", "adopt_lease@fsn-2"]);
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

  describe("C16", () => {
    it("lease_from_machines: one above the highest lease a Machine holds, adopted by every Machine with a Pool", async () => {
      machines = [
        { name: "fsn-1", pool: true, copy: writerCopy("10"), lease: { lease: 7, pos: { seq: 4, round: 0, sub: 5 }, cycle: "closed" }, warms: ["10"] },
        { name: "fsn-2", pool: true, copy: slotCopy("10"), lease: { lease: 3, pos: { seq: 4, round: 0, sub: 5 }, cycle: "closed" } },
        { name: "fsn-3", pool: false, copy: null, lease: null },
        { name: "docker-1", pool: false, stateless: true, copy: null, lease: null, silent: true },
      ];
      const run = await requested({ kind: "sync", args: { full: false } });

      const output = await execute(run.id);

      expect(output.error).toBeUndefined();
      expect(await rows()).toMatchObject([{ lease: "8", state: "done" }]);
      expect(verbs.filter((verb) => verb.startsWith("adopt_lease")).sort()).toEqual(["adopt_lease@fsn-1", "adopt_lease@fsn-2"]);
      expect(machines.map((fake) => fake.lease?.lease ?? null)).toEqual([8, 8, null, null]);
    });

    it("lease_above_records_after_db_reset: above an earlier run's lease when the Machines hold less", async () => {
      await harness.pool.query(
        `insert into volume_run (organization_id, environment_id, volume_id, volume_name, docker_volume, kind, args, state, lease, finished_at)
         values ($1, 'env-1', $2, 'data', $3, 'sync', '{"full":false}', 'done', 12, now())`,
        [organizationId, volumeId, dockerVolume]);
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy("10"), lease: null };
      first(machines).lease = { lease: 4, pos: { seq: 4, round: 0, sub: 5 }, cycle: "closed" };
      first(machines).warms = ["10"];
      const run = await requested({ kind: "sync", args: { full: false } });

      await execute(run.id);

      expect((await rows()).find((row) => row.id === run.id)).toMatchObject({ lease: "13", state: "done" });
      expect(machines[0]?.lease?.lease).toBe(13);
    });

    it("a replayed lease step adopts the lease the row already holds", async () => {
      const run = await requested({ kind: "sync", args: { full: false } });
      await harness.pool.query(`update volume_run set state = 'running', inngest_run_id = 'run-1', lease = 40 where id = $1`, [run.id]);
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy("10"), lease: null };
      first(machines).warms = ["10"];

      await execute(run.id, "run-1");

      expect(machines.map((fake) => fake.lease?.lease)).toEqual([40, 40]);
    });
  });

  describe("verbs per kind", () => {
    it("mirror: declares the slot, then rounds until Warm makes nothing new", async () => {
      first(machines).warms = ["21", "22", "22"];
      const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.error).toBeUndefined();
      expect(output.result).toEqual({ runId: run.id, rounds: 2 });
      expect(ordered(verbs)).toEqual([
        "adopt_lease@fsn-1", "adopt_lease@fsn-2",
        "declare_mirror@fsn-2(3,0,0)",
        "begin_round@fsn-2(4,0,0)", "warm_snapshot@fsn-1(4,0,2)", "start_receive@fsn-2(4,0,3)",
        "prune_mirror@fsn-2(4,0,4)", "commit_snapshots@fsn-1(4,0,5)",
        "begin_round@fsn-2(4,1,0)", "commit_snapshots@fsn-1(4,1,1)", "warm_snapshot@fsn-1(4,1,2)", "start_receive@fsn-2(4,1,3)",
        "prune_mirror@fsn-2(4,1,4)", "commit_snapshots@fsn-1(4,1,5)",
        "begin_round@fsn-2(4,2,0)", "commit_snapshots@fsn-1(4,2,1)", "warm_snapshot@fsn-1(4,2,2)",
      ]);
      expect(machines[1]?.copy).toMatchObject({ kind: "slot", newest: { guid: "22" } });
    });

    it("mirror: commits a guid above 2^53 exactly as the Machine reported it", async () => {
      const guid = "5695289101028938467";
      first(machines).warms = [guid];
      const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.error).toBeUndefined();
      expect(sent.flatMap((request) => request.command === "commit_snapshots" ? [request.payload.mirror_newest] : []))
        .toEqual([guid, guid]);
    });

    it("mirror: a resumable receive starts again from its token at the same position", async () => {
      first(machines).warms = ["21"];
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy(null), lease: null, receiving: { target: "ployz-21", guid: "21", polls: 0, resumable: true } };
      const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });

      await execute(run.id);

      expect(ordered(verbs).slice(2, 6)).toEqual([
        "begin_round@fsn-2(4,0,0)", "warm_snapshot@fsn-1(4,0,2)", "start_receive@fsn-2(4,0,3)", "start_receive@fsn-2(4,0,3)",
      ]);
      expect(await rows()).toMatchObject([{ state: "done" }]);
    });

    it("mirror: a Server without a Pool is refused before any lease", async () => {
      machines[1] = { name: "fsn-2", pool: false, copy: null, lease: null };
      const run = await requested({ kind: "mirror", args: { to: "fsn-2" } });

      const output = await execute(run.id);

      expect(output.result).toMatchObject({ refused: { code: "no_pool" } });
      expect(verbs).toEqual([]);
      expect(await rows()).toMatchObject([{ state: "failed", message: "fsn-2 has no managed volume storage yet", lease: null }]);
    });

    it("sync: rounds onto the existing mirror, committing what it already holds first", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy("10"), lease: null };
      first(machines).warms = ["21", "21"];
      const run = await requested({ kind: "sync", args: { full: false } });

      await execute(run.id);

      expect(ordered(verbs)).toEqual([
        "adopt_lease@fsn-1", "adopt_lease@fsn-2",
        "begin_round@fsn-2(4,0,0)", "commit_snapshots@fsn-1(4,0,1)", "warm_snapshot@fsn-1(4,0,2)", "start_receive@fsn-2(4,0,3)",
        "prune_mirror@fsn-2(4,0,4)", "commit_snapshots@fsn-1(4,0,5)",
        "begin_round@fsn-2(4,1,0)", "commit_snapshots@fsn-1(4,1,1)", "warm_snapshot@fsn-1(4,1,2)",
      ]);
    });

    it("sync --full: destroys the mirror, forgets the writer's snapshots, declares a fresh slot, then rounds", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy("10"), lease: null };
      first(machines).warms = ["21", "21"];
      const run = await requested({ kind: "sync", args: { full: true } });

      await execute(run.id);

      expect(ordered(verbs).slice(0, 7)).toEqual([
        "adopt_lease@fsn-1", "adopt_lease@fsn-2",
        "destroy_mirror@fsn-2(3,0,0)", "forget_snapshots@fsn-1(3,0,1)", "declare_mirror@fsn-2(3,0,2)",
        "begin_round@fsn-2(4,0,0)", "warm_snapshot@fsn-1(4,0,2)",
      ]);
      expect(await rows()).toMatchObject([{ state: "done" }]);
    });

    it("sync: a diverged mirror fails the run with the rebuild hint", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy("10"), lease: null };
      failNext.set("commit_snapshots@fsn-1", { code: "failed_precondition", message: "diverged", details: { reason: "precondition" } });
      const run = await requested({ kind: "sync", args: { full: false } });

      const output = await execute(run.id);

      expect(output.error).toBeDefined();
      expect(verbs.at(-1)).toBe("commit_snapshots@fsn-1(4,0,1)");
      expect(await rows()).toMatchObject([{
        state: "failed",
        message: `data-fsn-2 diverged at ${new Date(1_790_000_000 * 1000).toISOString()}; volume sync --full to rebuild`,
      }]);
    });

    it("delete_mirror: destroys the slot, then forgets on the writer", async () => {
      machines[1] = { name: "fsn-2", pool: true, copy: slotCopy("10"), lease: null };
      const run = await requested({ kind: "delete_mirror", args: { slot: "fsn-2", confirmed_name: null } });

      await execute(run.id);

      expect(ordered(verbs)).toEqual(["adopt_lease@fsn-1", "adopt_lease@fsn-2", "destroy_mirror@fsn-2(3,0,0)", "forget_snapshots@fsn-1(4,0,0)"]);
      expect(await rows()).toMatchObject([{ state: "done" }]);
    });

    it("orphan delete: destroys every slot of the name, forgets nothing", async () => {
      machines = [
        { name: "fsn-1", pool: true, copy: null, lease: null },
        { name: "fsn-2", pool: true, copy: slotCopy("10"), lease: null },
        { name: "fsn-3", pool: true, copy: slotCopy("9", "handed_in"), lease: null },
      ];
      const orphan = "gone-production_vol-old";
      const started = await runEffect(startOrphanDeletes(organizationId, () =>
        Effect.succeed([{ machine_id: idOf("fsn-2"), name: orphan, role: "slot" }, { machine_id: idOf("fsn-3"), name: orphan, role: "slot" }] as CopyObservation["copies"])).pipe(Effect.orDie));

      expect(started).toHaveLength(1);
      expect(await rows()).toMatchObject([{ kind: "delete_mirror", orphan: true, volume_id: "old", docker_volume: orphan, state: "requested" }]);

      const output = await execute(first(started), "run-1", undefined, "old");

      expect(output.error).toBeUndefined();
      expect(ordered(verbs)).toEqual([
        "adopt_lease@fsn-1", "adopt_lease@fsn-2", "adopt_lease@fsn-3", "destroy_mirror@fsn-2(3,0,0)", "destroy_mirror@fsn-3(3,0,0)",
      ]);
      expect(await rows()).toMatchObject([{ state: "done" }]);
    });
  });

  describe("requireOwner and SwitchError", () => {
    it("every effect step refuses a row this run no longer owns, before any verb", async () => {
      const run = await requested({ kind: "sync", args: { full: false } });
      await harness.pool.query(`update volume_run set state = 'cancelled', inngest_run_id = 'run-1' where id = $1`, [run.id]);

      const owned = await harness.runEffect(Effect.exit(requireOwner(run.id, "run-1")));
      const sent = await Effect.runPromiseExit(Effect.tryPromise(() =>
        runEffect(sendSwitch(context(run.id), "run-1", { id: idOf("fsn-1"), name: "fsn-1" }, () =>
          ({ command: "forget_snapshots", payload: { switch: { lease: 1, pos: { seq: 4, round: 0, sub: 0 }, not_after_unix_seconds: 1 }, name: dockerVolume } })))));

      expect(Exit.isFailure(owned)).toBe(true);
      expect(Exit.isFailure(sent)).toBe(true);
      expect(verbs).toEqual([]);
    });

    it.each([
      ["stale_lease", "a newer run took data's lease; this run stopped"],
      ["stale_step", "data's lease moved past this step"],
      ["volume_switching", "data is mid-run; wait or volume release data"],
      ["no_writer", "data has no writer on fsn-1"],
      ["no_capacity", "fsn-1 has no room for data's mirror"],
    ])("%s ends the run for good with its message", async (reason, message) => {
      const run = await requested({ kind: "sync", args: { full: false } });
      await harness.pool.query(`update volume_run set state = 'running', inngest_run_id = 'run-1' where id = $1`, [run.id]);
      failNext.set("forget_snapshots@fsn-1", { code: "failed_precondition", message: reason, details: { reason } });

      const failure = await runEffect(sendSwitch(context(run.id), "run-1", { id: idOf("fsn-1"), name: "fsn-1" }, () =>
        ({ command: "forget_snapshots", payload: { switch: { lease: 1, pos: { seq: 4, round: 0, sub: 0 }, not_after_unix_seconds: 1 }, name: dockerVolume } })))
        .then(() => null, (error: Error) => error);

      expect(failure).toBeInstanceOf(NonRetriableError);
      expect(await rows()).toMatchObject([{ state: "failed", message }]);
    });

    it.each(["busy", "expired"])("%s fails only the attempt, so the step retries", async (reason) => {
      const run = await requested({ kind: "sync", args: { full: false } });
      await harness.pool.query(`update volume_run set state = 'running', inngest_run_id = 'run-1' where id = $1`, [run.id]);
      failNext.set("forget_snapshots@fsn-1", { code: "unavailable", message: reason, details: { reason, skew_seconds: 3 } });

      const failure = await runEffect(sendSwitch(context(run.id), "run-1", { id: idOf("fsn-1"), name: "fsn-1" }, () =>
        ({ command: "forget_snapshots", payload: { switch: { lease: 1, pos: { seq: 4, round: 0, sub: 0 }, not_after_unix_seconds: 1 }, name: dockerVolume } })))
        .then(() => null, (error: Error) => error);

      expect(failure).toBeInstanceOf(Error);
      expect(failure).not.toBeInstanceOf(NonRetriableError);
      expect(await rows()).toMatchObject([{ state: "running", message: null }]);
    });

    it("a failure with no switch reason keeps the Machine's message", async () => {
      const run = await requested({ kind: "sync", args: { full: false } });
      await harness.pool.query(`update volume_run set state = 'running', inngest_run_id = 'run-1' where id = $1`, [run.id]);
      failNext.set("forget_snapshots@fsn-1", { code: "internal", message: "dataset is busy", details: null });

      const failure = await runEffect(sendSwitch(context(run.id), "run-1", { id: idOf("fsn-1"), name: "fsn-1" }, () =>
        ({ command: "forget_snapshots", payload: { switch: { lease: 1, pos: { seq: 4, round: 0, sub: 0 }, not_after_unix_seconds: 1 }, name: dockerVolume } })))
        .then(() => null, (error: Error) => error);

      expect(failure).not.toBeInstanceOf(NonRetriableError);
      expect(failure?.message).toBe("forget_snapshots on fsn-1: internal: dataset is busy");
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
