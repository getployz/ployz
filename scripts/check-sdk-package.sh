#!/usr/bin/env bash

set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"

cargo build -p ployz-sdk --locked

if [ -f target/debug/libployz_sdk.so ]; then
    cp target/debug/libployz_sdk.so ployz-sdk/ployz-sdk.node
elif [ -f target/debug/libployz_sdk.dylib ]; then
    cp target/debug/libployz_sdk.dylib ployz-sdk/ployz-sdk.node
elif [ -f target/debug/ployz_sdk.dll ]; then
    cp target/debug/ployz_sdk.dll ployz-sdk/ployz-sdk.node
else
    echo "ployz-sdk cdylib was not produced" >&2
    exit 1
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/node_modules/@ployz"
ln -s "$ROOT/ployz-sdk" "$TMP/node_modules/@ployz/sdk"

cd "$TMP"
node --input-type=commonjs <<'EOF'
const fs = require("node:fs");
const path = require("node:path");
const pkg = require("@ployz/sdk/package.json");
if (pkg.name !== "@ployz/sdk") {
  throw new Error(`expected npm name @ployz/sdk, got ${pkg.name}`);
}
if (pkg.private === true) {
  throw new Error("@ployz/sdk must be publishable");
}
if (!pkg.publishConfig || pkg.publishConfig.access !== "public") {
  throw new Error("@ployz/sdk must publish as a public scoped package");
}

const rootExport = pkg.exports && pkg.exports["."];
if (!rootExport || typeof rootExport !== "object") {
  throw new Error('package.json exports["."] is missing');
}
if (rootExport.types !== "./index.d.ts") {
  throw new Error(`types export must be ./index.d.ts, got ${rootExport.types}`);
}
if (rootExport.node !== "./index.js") {
  throw new Error(`node export must be ./index.js, got ${rootExport.node}`);
}
if (rootExport.browser !== "./browser.mjs") {
  throw new Error(`browser export must be ./browser.mjs, got ${rootExport.browser}`);
}
const pkgDir = path.dirname(require.resolve("@ployz/sdk/package.json"));
const browserPath = path.join(pkgDir, rootExport.browser);
if (!fs.existsSync(browserPath)) {
  throw new Error("browser.mjs is missing");
}

const sdk = require("@ployz/sdk");
const loader = fs.readFileSync(require.resolve("@ployz/sdk"), "utf8");
if (/\b__dirname\b|\b__filename\b/.test(loader)) {
  throw new Error(
    "@ployz/sdk must not use __dirname/__filename (Nitro ESM wrap does not define them)",
  );
}
if (typeof sdk.packageName !== "function") {
  throw new Error("packageName export is missing");
}
if (sdk.packageName() !== "@ployz/sdk") {
  throw new Error(`expected packageName() to return @ployz/sdk, got ${sdk.packageName()}`);
}
if (typeof sdk.connect !== "function") {
  throw new Error("connect export is missing");
}
if (typeof sdk.listHeld !== "function") {
  throw new Error("listHeld export is missing");
}
if (typeof sdk.register !== "function") {
  throw new Error("register export is missing");
}
if (typeof sdk.revokePairing !== "function") {
  throw new Error("revokePairing export is missing");
}
if (typeof sdk.Client !== "function") {
  throw new Error("Client export is missing");
}
if (typeof sdk.RpcError !== "function") {
  throw new Error("RpcError export is missing");
}

const facadePath = require.resolve("@ployz/sdk");
const Module = require("node:module");
const originalLoad = Module._load;
const unrelated = new Error(JSON.stringify({ code: "internal", message: "not an RPC error" }));
let isolatedSdk;
try {
  Module._load = function (request, parent) {
    if (request === "./ployz-sdk.node" && parent?.filename === facadePath) {
      return { connect: () => Promise.reject(unrelated) };
    }
    return originalLoad.apply(this, arguments);
  };
  delete require.cache[facadePath];
  isolatedSdk = require(facadePath);
} finally {
  Module._load = originalLoad;
  delete require.cache[facadePath];
}

const forbidden = [
  "call",
  "request",
  "watch",
  "preview",
  "deploy",
  "connectTcp",
  "connectSsh",
  "connectUnix",
  "connectSSH",
  "connectHeld",
];
for (const name of forbidden) {
  if (Object.hasOwn(sdk, name)) {
    throw new Error(`${name} must not be exported`);
  }
}

const dts = fs.readFileSync(require.resolve("@ployz/sdk/index.d.ts"), "utf8");
const payloads = fs.readFileSync(path.join(pkgDir, "generated/payloads.d.ts"), "utf8");
for (const [file, text] of [["index.d.ts", dts], ["generated/payloads.d.ts", payloads]]) {
  if (text.includes("Additive")) {
    throw new Error(`${file} must not reference Additive: an index signature disables excess-property checks`);
  }
}
if (!dts.includes("export declare function connect")) {
  throw new Error("index.d.ts is missing connect");
}
if (!dts.includes("export declare function listHeld")) {
  throw new Error("index.d.ts is missing listHeld");
}
if (!dts.includes("export declare function register")) {
  throw new Error("index.d.ts is missing register");
}
if (!dts.includes("RegisterRequest") || !dts.includes("Registered")) {
  throw new Error("index.d.ts is missing RegisterRequest / Registered");
}
if (!dts.includes("about(): Promise<ContractDescription>")) {
  throw new Error("index.d.ts is missing about()");
}
if (!dts.includes("preview(intent: DeployIntent): Promise<PreparedDeploy>")) {
  throw new Error("index.d.ts is missing preview()");
}
if (!dts.includes("run(") || !dts.includes("Promise<DeployOutcome<ExecutionError>>")) {
  throw new Error("index.d.ts is missing run()");
}
if (dts.includes("deploy(intent: DeployIntent)")) {
  throw new Error("index.d.ts must not declare unary deploy()");
}
if (!dts.includes("removeVolumes(")) {
  throw new Error("index.d.ts is missing removeVolumes()");
}
if (!dts.includes("RemoveVolumesRequest")) {
  throw new Error("index.d.ts is missing RemoveVolumesRequest");
}
if (!dts.includes("dataLossIfMachineRemoved(machine: MachineTarget): Promise<ObservedDataLoss>")) {
  throw new Error("index.d.ts is missing dataLossIfMachineRemoved()");
}
if (!dts.includes("close(): Promise<void>")) {
  throw new Error("index.d.ts is missing close()");
}
if (!dts.includes("readonly runtime:")) {
  throw new Error("index.d.ts is missing Client.runtime");
}
if (!dts.includes("watch(options?: WatchOptions): AsyncIterable<RuntimeWatchFrame>")) {
  throw new Error("index.d.ts is missing runtime.watch()");
}
for (const needle of ["connectSsh", "connectTcp", "connectUnix", "watchRuntime", "connectHeld"]) {
  if (dts.includes(needle)) {
    throw new Error(`index.d.ts must not declare ${needle}`);
  }
}
if (typeof sdk.Client.prototype.preview !== "function") {
  throw new Error("Client.preview must be a method");
}
if (typeof sdk.Client.prototype.run !== "function") {
  throw new Error("Client.run must be a method");
}
if (typeof sdk.Client.prototype.deploy !== "undefined") {
  throw new Error("Client.deploy must not exist");
}
if (typeof sdk.Client.prototype.register !== "undefined") {
  throw new Error("Client.register must not exist");
}
if (typeof sdk.applyAll !== "function" || typeof sdk.applyOne !== "function") {
  throw new Error("applyAll / applyOne must be exported");
}
const helperSpec = { name: "api" };
for (const intent of [
  sdk.applyAll("app", [helperSpec]),
  sdk.applyOne("app", helperSpec),
]) {
  if ("provisioned_volumes" in intent) {
    throw new Error("Deploy Intent helpers must not emit the removed provisioned_volumes field");
  }
}
if (typeof sdk.Client.prototype.removeVolumes !== "function") {
  throw new Error("Client.removeVolumes must be a method");
}
if (typeof sdk.Client.prototype.dataLossIfMachineRemoved !== "function") {
  throw new Error("Client.dataLossIfMachineRemoved must be a method");
}

async function expectRpc(fn, code) {
  try {
    await fn();
    throw new Error(`expected ${code}`);
  } catch (error) {
    if (!(error instanceof sdk.RpcError)) {
      throw new Error(`expected RpcError, got ${error && error.constructor.name}`);
    }
    if (error.code !== code) {
      throw new Error(`expected ${code}, got ${error.code}`);
    }
    if (!error.message || error.message.startsWith("{")) {
      throw new Error(`RpcError must expose its message directly, got ${error.message}`);
    }
  }
}

(async () => {
  try {
    await isolatedSdk.connect({});
    throw new Error("expected unrelated native error");
  } catch (error) {
    if (error !== unrelated || error instanceof isolatedSdk.RpcError) {
      throw new Error("JSON-shaped native errors must pass through unchanged");
    }
  }
  await expectRpc(
    () =>
      sdk.connect({
        relayUrl: "http://127.0.0.1:1",
        bearer: "",
        pairing: "pairing-secret",
        machineId: "0123456789abcdef0123456789abcdef",
      }),
    "unauthenticated",
  );
  await expectRpc(
    () =>
      sdk.connect({
        relayUrl: "http://127.0.0.1:1",
        bearer: "dial-secret",
        pairing: "pairing-secret",
        machineId: "not-a-machine-id",
      }),
    "invalid_argument",
  );
  console.log(`loaded ${path.join("@ployz", "sdk")} ${pkg.version}`);
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
EOF

cd "$ROOT/ployz-sdk"
if [ ! -d node_modules/typescript ]; then
  npm ci --ignore-scripts
fi
npx --no-install tsc --noEmit
