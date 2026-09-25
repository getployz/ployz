"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ployz-sdk-build-"));
fs.copyFileSync(path.join(process.env.PLOYZ_SDK_PACKAGE, "index.js"), path.join(dir, "index.js"));
fs.copyFileSync(path.join(process.env.PLOYZ_SDK_PACKAGE, "runtime-logs.js"), path.join(dir, "runtime-logs.js"));
fs.copyFileSync(process.env.PLOYZ_SDK_ADDON, path.join(dir, "ployz-sdk.node"));
const sdk = require(dir);

(async () => {
  const client = await sdk.connect({ connections: [{ unix: path.join(process.env.PLOYZ_SOCKET_DIRECTORY, `${process.env.PLOYZ_MACHINE_ID}.sock`) }] });
  try {
    const checkout = path.join(dir, "checkout");
    fs.mkdirSync(checkout);
    fs.writeFileSync(path.join(checkout, "Dockerfile"), "FROM scratch\n");
    const input = {
      deployment: {
        projectName: "app",
        snapshots: [{
          config: {
            version: 2, privateDns: "api",
            source: { version: 2, type: "git", repository: "acme/api", repositoryId: 42,
              access: { type: "public" }, rootDir: "/", branch: { type: "connected", name: "main" } },
            build: { buildMethod: "dockerfile", dockerfilePath: "Dockerfile", command: null },
            healthcheck: { type: "none" }, restartPolicy: "unless-stopped",
          },
        }],
      },
      sources: { api: checkout },
      source_commits: { api: "a".repeat(40) },
    };
    assert.throws(() => client.build({ ...input, unexpected: true }),
      error => error instanceof sdk.RpcError && error.code === "invalid_argument");
    const build = client.build(input, { startWithinMs: 60000 });
    const outcome = await build.finished;
    assert.equal(outcome.kind, "built", JSON.stringify(outcome));
    assert.match(outcome.receipt.fingerprint, /^[0-9a-f]{64}$/);
    assert.match(outcome.receipt.image.reference, /^sha256:/);
    const stages = [];
    for await (const event of build) if (event.Build?.Stage) stages.push(event.Build.Stage);
    assert.ok(stages.includes("Upload"), JSON.stringify(stages));
  } finally {
    client.close();
    fs.rmSync(dir, { recursive: true, force: true });
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
