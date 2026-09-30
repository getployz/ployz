"use strict";

// Store seams Cloud's GitHub workers call refuse malformed input before any read.
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const expectRpcError = require("./expect-rpc-error");

const addon = process.env.PLOYZ_SDK_ADDON;
const pkg = process.env.PLOYZ_SDK_PACKAGE;
if (!addon || !pkg) {
  throw new Error("Node Store smoke is missing environment");
}

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ployz-sdk-store-"));
fs.copyFileSync(path.join(pkg, "index.js"), path.join(dir, "index.js"));
fs.copyFileSync(path.join(pkg, "runtime-logs.js"), path.join(dir, "runtime-logs.js"));
fs.copyFileSync(addon, path.join(dir, "ployz-sdk.node"));
const sdk = require(dir);

async function expectInvalid(what, fn) {
  try {
    await fn();
  } catch (error) {
    const rpc = expectRpcError(sdk, error);
    if (rpc.code !== "invalid_argument") {
      throw new Error(`${what}: expected invalid_argument, got ${rpc.code}: ${rpc.message}`);
    }
    return;
  }
  throw new Error(`${what}: expected a refusal`);
}

(async () => {
  const store = await sdk.openConfigStore(
    `sqlite:${path.join(dir, "store.db")}`,
    "a sealing secret long enough for tests",
  );
  for (const call of ["branchHead", "pendingSaves"]) {
    await expectInvalid(`${call} organization`, () => store[call]("Not An Id!", 1, "main"));
    await expectInvalid(`${call} repository`, () => store[call]("acme", -1, "main"));
    await expectInvalid(`${call} branch`, () => store[call]("acme", 1, "no spaces allowed"));
  }
  // Well-formed input reads: nothing seen yet.
  if ((await store.branchHead("acme", 1, "main")) !== null) {
    throw new Error("an unseen branch has no head");
  }
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
