"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ployz-sdk-prepare-"));
fs.copyFileSync(path.join(process.env.PLOYZ_SDK_PACKAGE, "index.js"), path.join(dir, "index.js"));
fs.copyFileSync(process.env.PLOYZ_SDK_ADDON, path.join(dir, "ployz-sdk.node"));
const sdk = require(dir);

(async () => {
  const client = await sdk.connect({ connections: [{ unix: path.join(process.env.PLOYZ_SOCKET_DIRECTORY, `${process.env.PLOYZ_MACHINE_ID}.sock`) }] });
  try {
    const checkout = path.join(dir, "checkout");
    fs.mkdirSync(path.join(checkout, "apps/api"), { recursive: true });
    fs.writeFileSync(path.join(checkout, "Dockerfile"), "FROM scratch\n");
    const input = {
      deployment: {
        projectName: "app",
        snapshots: [{
          config: {
            version: 2, privateDns: "api",
            source: { version: 2, type: "git", repository: "acme/api", repositoryId: 42,
              installationId: 7, rootDir: "/apps/api", branch: { type: "connected", name: "main" } },
            build: { builder: "dockerfile", dockerfilePath: "../../Dockerfile" },
            preDeployCommand: null, startCommand: null,
            healthcheck: { type: "none" }, restartPolicy: "unless-stopped",
          },
          resolvedEnv: { PUBLIC_BUILD_VALUE: "frozen" },
        }],
        volumes: [],
      },
      sources: { api: checkout },
    };
    const cancelled = new AbortController();
    cancelled.abort();
    assert.throws(() => client.prepare(input, { signal: cancelled.signal }),
      error => error.name === "AbortError");
    const preparation = client.prepare(input);
    if (process.env.PLOYZ_PREPARATION_OUTCOME === "success") {
      const prepared = await preparation.finished;
      const events = [];
      for await (const event of preparation) events.push(event);
      assert.ok(events.some(event => event.Delivered), "image delivery finishes before confirmation");
      assert.ok(prepared.operations.length > 0);
      const running = prepared.confirm();
      assert.throws(() => prepared.confirm(), "confirmation is single-use");
      const outcome = await running.finished;
      assert.equal(outcome.type, "success", JSON.stringify(outcome));
    } else if (process.env.PLOYZ_PREPARATION_OUTCOME === "cancel") {
      let cancelled = false;
      for await (const event of preparation) {
        if (event.Build?.Stage === "Building") {
          preparation.abort();
          cancelled = true;
        }
      }
      assert.ok(cancelled, "quiet execution must remain cancellable");
      await assert.rejects(preparation.finished, error => {
        assert.ok(error instanceof sdk.RpcError);
        assert.equal(error.details.preparation.kind, "cancelled");
        assert.equal(error.details.preparation.stage, "Building");
        assert.deepEqual(error.details.preparation.work, { api: "Unattempted" });
        assert.match(error.message, /cleanup complete/);
        return true;
      });
    } else if (process.env.PLOYZ_PREPARATION_OUTCOME === "selection") {
      await assert.rejects(preparation.finished, error => {
        assert.equal(error.details.preparation.kind, "failed");
        assert.equal(error.details.preparation.stage, "Selection");
        assert.equal(error.details.preparation.rejections["builds disabled"], 1);
        assert.match(error.message, /no build was started/);
        return true;
      });
    } else {
      // Terminal completion must not depend on draining the progress iterator.
      await assert.rejects(preparation.finished, error => {
        assert.ok(error instanceof sdk.RpcError);
        assert.equal(error.details.preparation.kind, process.env.PLOYZ_PREPARATION_OUTCOME);
        assert.equal(error.details.preparation.stage, "Admission");
        assert.deepEqual(error.details.preparation.work, { api: "Unattempted" });
        return true;
      });
      const events = [];
      for await (const event of preparation) events.push(event);
      assert.ok(events.length > 0, "buffered progress remains readable after rejection");
    }
    assert.ok(await client.about(), "failed preparation must not close the client");
  } finally {
    client.close();
    fs.rmSync(dir, { recursive: true, force: true });
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
