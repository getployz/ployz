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
const sdk = require("@ployz/sdk");
if (typeof sdk.packageName !== "function") {
  throw new Error("packageName export is missing");
}
if (sdk.packageName() !== "@ployz/sdk") {
  throw new Error(`expected packageName() to return @ployz/sdk, got ${sdk.packageName()}`);
}
if (typeof sdk.connect !== "function") {
  throw new Error("connect export is missing");
}
if (typeof sdk.Client !== "function") {
  throw new Error("Client export is missing");
}

const forbidden = [
  "call",
  "request",
  "watch",
  "deploy",
  "connectTcp",
  "connectSsh",
  "connectUnix",
  "connectSSH",
];
for (const name of forbidden) {
  if (Object.hasOwn(sdk, name)) {
    throw new Error(`${name} must not be exported`);
  }
}

const dts = fs.readFileSync(require.resolve("@ployz/sdk/index.d.ts"), "utf8");
if (!dts.includes("export declare function connect")) {
  throw new Error("index.d.ts is missing connect");
}
if (!dts.includes("about(): Promise<ContractDescription>")) {
  throw new Error("index.d.ts is missing about()");
}
if (!dts.includes("deploy(intent: DeployIntent): Promise<DeployOutcome<ExecutionError>>")) {
  throw new Error("index.d.ts is missing deploy()");
}
if (!dts.includes("close(): Promise<void>")) {
  throw new Error("index.d.ts is missing close()");
}
for (const needle of ["connectSsh", "connectTcp", "connectUnix", "watch("]) {
  if (dts.includes(needle)) {
    throw new Error(`index.d.ts must not declare ${needle}`);
  }
}
if (typeof sdk.Client.prototype.deploy !== "function") {
  throw new Error("Client.deploy must be a method");
}

async function expectRpc(fn, code) {
  try {
    await fn();
    throw new Error(`expected ${code}`);
  } catch (error) {
    let rpc;
    try {
      rpc = JSON.parse(error.message);
    } catch {
      throw new Error(`expected generated RpcError JSON, got ${error && error.message}`);
    }
    if (rpc.code !== code) {
      throw new Error(`expected ${code}, got ${rpc.code}`);
    }
  }
}

(async () => {
  await expectRpc(
    () =>
      sdk.connect({
        relayUrl: "http://127.0.0.1:1",
        bearer: "",
        machineId: "0123456789abcdef0123456789abcdef",
      }),
    "unauthenticated",
  );
  await expectRpc(
    () =>
      sdk.connect({
        relayUrl: "http://127.0.0.1:1",
        bearer: "dial-secret",
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
