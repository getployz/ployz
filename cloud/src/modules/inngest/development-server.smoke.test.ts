import { spawn, type ChildProcess } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer as createHttpServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createServer } from "inngest/node";
import { Inngest } from "inngest";
import { afterEach, describe, expect, it } from "vitest";

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

async function listen(server: Server, port: number) {
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, "127.0.0.1", resolve);
  });
}

async function closeServer(server: Server | undefined) {
  if (!server?.listening) return;
  await new Promise<void>((resolve) => server.close(() => resolve()));
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
  let appServer: Server | undefined;
  let devServer: ChildProcess | undefined;
  let directory: string | undefined;

  afterEach(async () => {
    await closeServer(appServer);
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

    const appPort = await availablePort();
    const devPort = await availablePort();
    const gatewayPort = await availablePort();
    const gatewayGrpcPort = await availablePort();
    const executorGrpcPort = await availablePort();
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

    const cancellation = inngest.createFunction(
      {
        id: "development-server-cancellation-observer",
        triggers: [{ event: "inngest/function.cancelled" }],
      },
      async ({ event, step }) => {
        const runId = String(event.data["run_id"]);
        await step.run("terminalize-cancelled-operation", () =>
          store.updateByRunId(runId, (row) => ({
            ...row,
            status: "cancelled",
          })),
        );
        return { runId, status: "cancelled" as const };
      },
    );

    appServer = createServer({
      client: inngest,
      functions: [resumed, cancellable, cancellation],
      serveOrigin: `http://127.0.0.1:${appPort}`,
    });
    await listen(appServer, appPort);

    devServer = spawn(
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
        "--sdk-url",
        `http://127.0.0.1:${appPort}/api/inngest`,
      ],
      { stdio: ["ignore", "pipe", "pipe"] },
    );

    await waitForDevServer(devPort, devServer);
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
  }, 60_000);
});

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
