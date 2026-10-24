import { execFile as execFileCallback } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { createServer, type IncomingMessage } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";

import { afterAll, beforeAll, describe, expect, it } from "vitest";

const execFile = promisify(execFileCallback);
const dashboard = new URL("../..", import.meta.url).pathname;

// The app as web starts it: a named ESM import of node:http, which only the loader hook can instrument.
// It exits by draining, so the SDK flushes its spans on beforeExit before the test reads the collector.
const app = `
import { createServer, request } from "node:http";
const server = createServer((_, response) => response.end("ok")).listen(0, () => {
  request({ port: server.address().port }, (response) => {
    response.resume();
    response.on("end", () => {
      console.log(JSON.stringify({ sdk: Symbol.for("opentelemetry.js.api.1") in globalThis }));
      server.close();
    });
  }).end();
});
`;

let directory: string;
let entry: string;

beforeAll(async () => {
  directory = await mkdtemp(join(tmpdir(), "ployz-telemetry-"));
  entry = join(directory, "app.mjs");
  await writeFile(entry, app);
});

afterAll(async () => {
  await rm(directory, { recursive: true, force: true });
});

async function start(env: Record<string, string>) {
  const { stdout } = await execFile(process.execPath, ["--import", "./scripts/telemetry.ts", entry], {
    cwd: dashboard,
    env: {
      PATH: process.env["PATH"] ?? "",
      OTEL_NODE_RESOURCE_DETECTORS: "env",
      ...env,
    },
    timeout: 20_000,
  });
  // A console exporter prints spans too; the app's own line is the one that starts with {"sdk".
  const line = stdout.split("\n").find((output) => output.startsWith('{"sdk"'));
  return JSON.parse(line ?? "{}") as { sdk: boolean };
}

describe("web's telemetry bootstrap", () => {
  it("loads no OpenTelemetry SDK when nothing configures it, as on self-host", { timeout: 30_000 }, async () => {
    expect(await start({})).toEqual({ sdk: false });
  });

  // Hosted web names only the logs and metrics exporters. Even set to none, they start the SDK as before,
  // so traces still go to OTLP's default endpoint. That export to a closed port takes a few seconds.
  it("starts the SDK when any OpenTelemetry setting is given, as on hosted Cloud", { timeout: 30_000 }, async () => {
    expect(await start({ OTEL_LOGS_EXPORTER: "none", OTEL_METRICS_EXPORTER: "none" })).toEqual({ sdk: true });
  });

  it("starts the SDK for an OpenTelemetry setting given empty, as the old flags did", { timeout: 30_000 }, async () => {
    expect(await start({ OTEL_LOGS_EXPORTER: "", OTEL_METRICS_EXPORTER: "" })).toEqual({ sdk: true });
  });

  it("exports the app's spans when an OTLP endpoint is set", { timeout: 30_000 }, async () => {
    const received: string[] = [];
    const collector = createServer((request: IncomingMessage, response) => {
      received.push(`${request.method} ${request.url}`);
      request.resume();
      request.on("end", () => response.end());
    });
    await new Promise<void>((resolve) => collector.listen(0, "127.0.0.1", resolve));
    const { port } = collector.address() as AddressInfo;
    try {
      expect(
        await start({
          OTEL_EXPORTER_OTLP_ENDPOINT: `http://127.0.0.1:${port}`,
          OTEL_METRICS_EXPORTER: "none",
          OTEL_LOGS_EXPORTER: "none",
        }),
      ).toEqual({ sdk: true });
      expect(received).toContain("POST /v1/traces");
    } finally {
      collector.close();
    }
  });
});
