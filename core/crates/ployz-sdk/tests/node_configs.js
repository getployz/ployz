"use strict";

const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const expectRpcError = require("./expect-rpc-error");

const addon = process.env.PLOYZ_SDK_ADDON;
const pkg = process.env.PLOYZ_SDK_PACKAGE;
if (!addon || !pkg) {
  throw new Error("Node Configs smoke is missing environment");
}

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ployz-sdk-configs-"));
fs.copyFileSync(path.join(pkg, "index.js"), path.join(dir, "index.js"));
fs.copyFileSync(path.join(pkg, "runtime-logs.js"), path.join(dir, "runtime-logs.js"));
fs.copyFileSync(addon, path.join(dir, "ployz-sdk.node"));
const sdk = require(dir);

const org = "acme";
const environment = { project: "shop" };

async function expectCode(what, code, fn) {
  try {
    await fn();
  } catch (error) {
    const rpc = expectRpcError(sdk, error);
    assert.equal(rpc.code, code, `${what}: ${rpc.message}`);
    return rpc;
  }
  throw new Error(`${what}: expected ${code}`);
}

(async () => {
  const store = await sdk.openConfigStore(
    `sqlite:${path.join(dir, "store.db")}`,
    "a sealing secret long enough for tests",
  );
  const write = (command) => store.write(org, command);
  await write({
    command: "create_project",
    id: crypto.randomUUID(),
    name: "shop",
    default_environment: crypto.randomUUID(),
  });
  for (const name of ["web", "api"]) {
    await write({ command: "create_service", id: crypto.randomUUID(), environment, name, image: "nginx" });
  }
  await write({
    command: "create_config",
    id: crypto.randomUUID(),
    environment,
    name: "sentry",
    mounts: [{ service: "web", dir: "/etc/sentry" }],
  });
  const put = await write({
    command: "put_config_file",
    environment,
    config: "sentry",
    file: "conf.d/sentry.yml",
    content: "url: http://web:${{ api.PORT }}\n",
    mode: "0644",
  });
  assert.deepEqual(put.config.files.map((file) => [file.name, file.mode, file.references]), [
    ["conf.d/sentry.yml", "0644", ["api"]],
  ]);

  const listed = await store.read(org, { query: "configs", environment });
  assert.equal(listed.view, "configs");
  const [sentry] = listed.configs;
  assert.equal(sentry.name, "sentry");
  assert.deepEqual(sentry.mounts, [{ service: "web", dir: "/etc/sentry" }]);
  const one = await store.read(org, { query: "config", environment, config: "sentry" });
  assert.equal(one.contents["conf.d/sentry.yml"], "url: http://web:${{ api.PORT }}\n");

  const bare = await expectCode("bare reference", "invalid_argument", () =>
    write({ command: "put_config_file", environment, config: "sentry", file: "a", content: "${{ PORT }}" }),
  );
  assert.match(bare.message, /Configs are shared/);
  await expectCode("mount collision", "conflict", async () => {
    await write({ command: "create_config", id: crypto.randomUUID(), environment, name: "other", mounts: [] });
    await write({ command: "attach_config", environment, service: "web", config: "other", dir: "/etc/sentry" });
  });
  await expectCode("bad file name", "invalid_argument", () =>
    write({ command: "put_config_file", environment, config: "sentry", file: "../escape", content: "" }),
  );

  await write({ command: "delete_config", environment, config: "sentry" });
  const after = await store.read(org, { query: "configs", environment });
  assert.deepEqual(after.configs.map((config) => config.name), ["other"]);
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
