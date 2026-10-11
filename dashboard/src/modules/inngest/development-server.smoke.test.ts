import { spawn, type ChildProcess } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer as createHttpServer } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { connect, type WorkerConnection } from "inngest/connect";
import { Inngest } from "inngest";
import { afterEach, describe, expect, it } from "vitest";
import { DRAIN_SLOT } from "#/modules/inngest/drain-slot";
import { serverDrainRequestedEvent, serverPolicyChangeRequestedEvent } from "#/modules/inngest/events";
import type { ServerPolicyChange } from "#/modules/machines/server-policy";
import { createVolumeRunFunctions } from "#/modules/volume-run/volume-run.inngest";

type SmokeRow = {
  readonly operationId: string;
  readonly runId: string | null;
  readonly status: "pending" | "running" | "completed" | "cancelled";
  readonly claimCount: number;
  readonly interruptionCount: number;
};

class SmokeStore {
  private pending: Promise<void> = Promise.resolve();

  constructor(private readonly path: string) {}

  async initialize(rows: ReadonlyArray<SmokeRow>) {
    await writeFile(this.path, JSON.stringify(rows), "utf8");
  }

  async read(operationId: string) {
    await this.pending;
    const rows: ReadonlyArray<SmokeRow> = JSON.parse(
      await readFile(this.path, "utf8"),
    );
    return rows.find((row) => row.operationId === operationId) ?? null;
  }

  update(
    operationId: string,
    change: (current: SmokeRow) => SmokeRow,
  ): Promise<void> {
    this.pending = this.pending.then(async () => {
      const rows: ReadonlyArray<SmokeRow> = JSON.parse(
        await readFile(this.path, "utf8"),
      );
      const next = rows.map((row) =>
        row.operationId === operationId ? change(row) : row,
      );
      await writeFile(this.path, JSON.stringify(next), "utf8");
    });
    return this.pending;
  }

  async updateByRunId(
    runId: string,
    change: (current: SmokeRow) => SmokeRow,
  ) {
    const rows: ReadonlyArray<SmokeRow> = JSON.parse(
      await readFile(this.path, "utf8"),
    );
    const owned = rows.find((row) => row.runId === runId);
    if (owned) await this.update(owned.operationId, change);
  }
}

async function availablePort() {
  const server = createHttpServer();
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address() as AddressInfo | null;
  if (address === null) {
    throw new Error("Could not allocate an Inngest smoke-test port.");
  }
  await new Promise<void>((resolve, reject) =>
    server.close((error) => (error ? reject(error) : resolve())),
  );
  return address.port;
}

async function stopProcess(process: ChildProcess | undefined) {
  if (!process || process.exitCode !== null) return;
  process.kill("SIGTERM");
  await Promise.race([
    new Promise<void>((resolve) => process.once("exit", () => resolve())),
    new Promise<void>((resolve) => setTimeout(resolve, 2_000)),
  ]);
  if (process.exitCode === null) process.kill("SIGKILL");
}

async function waitForRow(
  store: SmokeStore,
  operationId: string,
  predicate: (row: SmokeRow) => boolean,
) {
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    const row = await store.read(operationId);
    if (row && predicate(row)) return row;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`Inngest smoke row ${operationId} did not reach its target.`);
}

describe("Inngest development server durable smoke", () => {
  let worker: WorkerConnection | undefined;
  let replacement: WorkerConnection | undefined;
  let releaseStep: (() => void) | undefined;
  let devServer: ChildProcess | undefined;
  let directory: string | undefined;

  afterEach(async () => {
    releaseStep?.();
    await worker?.close();
    await replacement?.close();
    await stopProcess(devServer);
    if (directory) await rm(directory, { recursive: true, force: true });
  });

  it("resumes an interrupted step and terminalizes a cancelled owned run", async () => {
    directory = await mkdtemp(join(tmpdir(), "ployz-inngest-smoke-"));
    const store = new SmokeStore(join(directory, "rows.json"));
    const resumedOperationId = "resume-operation";
    const cancelledOperationId = "cancel-operation";
    await store.initialize([
      {
        operationId: resumedOperationId,
        runId: null,
        status: "pending",
        claimCount: 0,
        interruptionCount: 0,
      },
      {
        operationId: cancelledOperationId,
        runId: null,
        status: "pending",
        claimCount: 0,
        interruptionCount: 0,
      },
    ]);

    const started = await startDevServer();
    devServer = started.process;
    const { devPort, gatewayPort } = started;
    const inngest = new Inngest({
      id: "ployz-development-server-smoke",
      eventKey: "local",
      baseUrl: `http://127.0.0.1:${devPort}`,
      isDev: true,
    });

    const resumed = inngest.createFunction(
      {
        id: "development-server-resume-smoke",
        retries: 2,
        triggers: [{ event: "smoke/resume" }],
        concurrency: [{ key: "event.data.operationId", limit: 1 }],
      },
      async ({ event, step, runId }) => {
        const operationId = String(event.data["operationId"]);
        await step.run("claim-resume-operation", () =>
          store.update(operationId, (row) => ({
            ...row,
            runId,
            status: "running",
            claimCount: row.claimCount + 1,
          })),
        );
        await step.run("interrupt-once", async () => {
          const row = await store.read(operationId);
          if (row?.interruptionCount === 0) {
            await store.update(operationId, (current) => ({
              ...current,
              interruptionCount: current.interruptionCount + 1,
            }));
            throw new Error("simulated application interruption");
          }
        });
        await step.run("complete-resumed-operation", () =>
          store.update(operationId, (row) => ({
            ...row,
            status: "completed",
          })),
        );
        return { operationId, status: "completed" as const };
      },
    );

    const cancellable = inngest.createFunction(
      {
        id: "development-server-cancellation-smoke",
        retries: 2,
        triggers: [{ event: "smoke/cancellable" }],
        cancelOn: [{ event: "smoke/cancel", match: "data.operationId" }],
        concurrency: [{ key: "event.data.operationId", limit: 1 }],
      },
      async ({ event, step, runId }) => {
        const operationId = String(event.data["operationId"]);
        await step.run("claim-cancellable-operation", () =>
          store.update(operationId, (row) => ({
            ...row,
            runId,
            status: "running",
            claimCount: row.claimCount + 1,
          })),
        );
        await step.sleep("hold-cancellable-operation", "30s");
        await step.run("complete-cancellable-operation", () =>
          store.update(operationId, (row) => ({
            ...row,
            status: "completed",
          })),
        );
      },
    );

    let cancelledFunctionId: unknown;
    const cancellation = inngest.createFunction(
      {
        id: "development-server-cancellation-observer",
        triggers: [{ event: "inngest/function.cancelled" }],
      },
      async ({ event, step }) => {
        const runId = String(event.data["run_id"]);
        cancelledFunctionId = event.data["function_id"];
        await step.run("terminalize-cancelled-operation", () =>
          store.updateByRunId(runId, (row) => ({
            ...row,
            status: "cancelled",
          })),
        );
        return { runId, status: "cancelled" as const };
      },
    );

    let startedResolve = () => {};
    const startedPromise = new Promise<void>((resolve) => { startedResolve = resolve; });
    let releaseResolve = () => {};
    const releasePromise = new Promise<void>((resolve) => { releaseResolve = resolve; });
    releaseStep = releaseResolve;
    let executions = 0;
    const draining = inngest.createFunction({ id: "drain-smoke", retries: 0,
      triggers: [{ event: "smoke/drain" }] }, async ({ step }) => {
      await step.run("held-work", async () => {
        executions++;
        startedResolve();
        await releasePromise;
      });
    });
    const functions = [resumed, cancellable, cancellation, draining];

    worker = await connect({ apps: [{ client: inngest, functions }],
      gatewayUrl: `ws://127.0.0.1:${gatewayPort}/v0/connect`, handleShutdownSignals: [] });
    await inngest.send({
      name: "smoke/resume",
      data: { operationId: resumedOperationId },
    });
    const resumedRow = await waitForRow(
      store,
      resumedOperationId,
      (row) => row.status === "completed",
    );
    expect(resumedRow).toEqual({
      operationId: resumedOperationId,
      runId: expect.any(String),
      status: "completed",
      claimCount: 1,
      interruptionCount: 1,
    });

    await inngest.send({
      name: "smoke/cancellable",
      data: { operationId: cancelledOperationId },
    });
    const runningRow = await waitForRow(
      store,
      cancelledOperationId,
      (row) => row.status === "running",
    );
    expect(runningRow.runId).toEqual(expect.any(String));
    await inngest.send({
      name: "smoke/cancel",
      data: { operationId: cancelledOperationId },
    });
    const cancelledRow = await waitForRow(
      store,
      cancelledOperationId,
      (row) => row.status === "cancelled",
    );
    expect(cancelledRow).toEqual({
      operationId: cancelledOperationId,
      runId: runningRow.runId,
      status: "cancelled",
      claimCount: 1,
      interruptionCount: 0,
    });
    expect(cancelledFunctionId).toBe("ployz-development-server-smoke-development-server-cancellation-smoke");

    await inngest.send({ name: "smoke/drain", data: {} });
    await startedPromise;
    let closed = false;
    const closing = worker.close().then(() => { closed = true; });
    replacement = await connect({ apps: [{ client: inngest, functions }],
      gatewayUrl: `ws://127.0.0.1:${gatewayPort}/v0/connect`, handleShutdownSignals: [] });
    expect(closed).toBe(false);
    await inngest.send({ name: "smoke/drain", data: {} });
    await expect.poll(() => executions, { timeout: 10_000 }).toBe(2);
    expect(closed).toBe(false);
    releaseResolve();
    await closing;
    expect(closed).toBe(true);
  }, 60_000);

  it("holds a policy change that turns services on behind its Organization's Drain, and no other change", async () => {
    const started = await startDevServer();
    devServer = started.process;
    const inngest = new Inngest({
      id: "ployz-drain-slot-smoke",
      eventKey: "local",
      baseUrl: `http://127.0.0.1:${started.devPort}`,
      isDev: true,
    });
    const running = new Set<string>();
    let release = () => {};
    const released = new Promise<void>((resolve) => { release = resolve; });
    releaseStep = release;
    const drain = inngest.createFunction(
      { id: "drain-slot-smoke-drain", retries: 0, triggers: [{ event: serverDrainRequestedEvent }], concurrency: [DRAIN_SLOT] },
      async ({ event, step }) => {
        await step.run("hold-drain", async () => {
          running.add(`drain ${String(event.data["organizationId"])}`);
          await released;
        });
      },
    );
    const policy = inngest.createFunction(
      { id: "drain-slot-smoke-policy", retries: 0, triggers: [{ event: serverPolicyChangeRequestedEvent }], concurrency: [DRAIN_SLOT] },
      async ({ event, step }) => {
        await step.run("apply-policy", () => { running.add(`policy ${String(event.data["machineId"])}`); });
      },
    );
    worker = await connect({ apps: [{ client: inngest, functions: [drain, policy] }],
      gatewayUrl: `ws://127.0.0.1:${started.gatewayPort}/v0/connect`, handleShutdownSignals: [] });
    const change = (organizationId: string, machineId: string, change: ServerPolicyChange) =>
      inngest.send({ name: serverPolicyChangeRequestedEvent, data: { organizationId, machineId, change } });

    await inngest.send({ name: serverDrainRequestedEvent, data: { attemptId: "a1", organizationId: "org-1", machineId: "m1" } });
    await expect.poll(() => running.has("drain org-1"), { timeout: 10_000 }).toBe(true);

    await change("org-1", "m1", { acceptsBuilds: false });
    await change("org-1", "m2", { acceptsServices: false });
    await change("org-1", "m3", { acceptsServices: true });
    await change("org-2", "m4", { acceptsServices: true });
    await expect.poll(() => ["policy m1", "policy m2", "policy m4"].every((key) => running.has(key)), { timeout: 10_000 }).toBe(true);
    await new Promise((resolve) => setTimeout(resolve, 1_000));
    expect(running.has("policy m3")).toBe(false);

    release();
    await expect.poll(() => running.has("policy m3"), { timeout: 10_000 }).toBe(true);
  }, 60_000);

  it("run_volume_singleton_skips: a second run of one Volume is skipped while the first sleeps and while it awaits a retry", async () => {
    const started = await startDevServer();
    devServer = started.process;
    const inngest = new Inngest({
      id: "ployz-volume-singleton-smoke",
      eventKey: "local",
      baseUrl: `http://127.0.0.1:${started.devPort}`,
      isDev: true,
    });
    const { singleton } = createVolumeRunFunctions(new Inngest({ id: "test" }))[0].opts;
    const claims: string[] = [];
    let sleeping = false;
    let failures = 0;
    let finished = false;
    const run = inngest.createFunction(
      { id: "volume-singleton-smoke", retries: 2, triggers: [{ event: "smoke/volume-run" }], singleton },
      async ({ event, step }) => {
        await step.run("claim", () => { claims.push(String(event.data["runId"])); });
        sleeping = true;
        await step.sleep("hold", "3s");
        sleeping = false;
        await step.run("flaky", () => {
          if (failures < 2) {
            failures += 1;
            throw new Error("simulated busy Machine");
          }
        });
        await step.run("finish", () => { finished = true; });
      },
    );
    worker = await connect({ apps: [{ client: inngest, functions: [run] }],
      gatewayUrl: `ws://127.0.0.1:${started.gatewayPort}/v0/connect`, handleShutdownSignals: [] });
    const send = (runId: string) => inngest.send({ name: "smoke/volume-run", data: { volumeId: "vol-1", runId } });

    await send("first");
    await expect.poll(() => sleeping, { timeout: 10_000 }).toBe(true);
    await send("while-sleeping");
    await expect.poll(() => failures, { timeout: 15_000 }).toBeGreaterThanOrEqual(1);
    await send("while-retrying");
    await expect.poll(() => finished, { timeout: 20_000 }).toBe(true);
    await new Promise((resolve) => setTimeout(resolve, 1_500));

    expect(claims).toEqual(["first"]);
  }, 60_000);
});

async function startDevServer() {
  const devPort = await availablePort();
  const gatewayPort = await availablePort();
  const gatewayGrpcPort = await availablePort();
  const executorGrpcPort = await availablePort();
  const devServer = spawn(
    join(process.cwd(), "node_modules/inngest-cli/bin/inngest"),
    [
      "dev",
      "--no-discovery",
      "--no-poll",
      "--port",
      String(devPort),
      "--connect-gateway-port",
      String(gatewayPort),
      "--connect-gateway-grpc-port",
      String(gatewayGrpcPort),
      "--connect-executor-grpc-port",
      String(executorGrpcPort),
      "--retry-interval",
      "1",
      "--tick",
      "50",
    ],
    { stdio: ["ignore", "pipe", "pipe"] },
  );
  await waitForDevServer(devPort, devServer);
  return { process: devServer, devPort, gatewayPort };
}

async function waitForDevServer(port: number, process: ChildProcess) {
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    if (process.exitCode !== null) {
      throw new Error(`Inngest development server exited with ${process.exitCode}.`);
    }
    try {
      const response = await fetch(`http://127.0.0.1:${port}`);
      if (response.ok) return;
    } catch {
      // The development server has not opened its HTTP listener yet.
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error("Inngest development server did not become ready.");
}
