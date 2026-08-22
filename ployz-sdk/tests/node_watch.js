"use strict";

const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { AbortController } = globalThis;

const addon = process.env.PLOYZ_SDK_ADDON;
const pkg = process.env.PLOYZ_SDK_PACKAGE;
const relayUrl = process.env.PLOYZ_RELAY_URL;
const bearer = process.env.PLOYZ_BEARER;
const pairing = process.env.PLOYZ_PAIRING;
const machineId = process.env.PLOYZ_MACHINE_ID;

if (!addon || !pkg || !relayUrl || !bearer || !pairing || !machineId) {
  throw new Error("Node Watch smoke is missing environment");
}

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ployz-sdk-watch-"));
fs.copyFileSync(path.join(pkg, "index.js"), path.join(dir, "index.js"));
fs.copyFileSync(addon, path.join(dir, "ployz-sdk.node"));
const sdk = require(dir);

function parseRpc(error) {
  try {
    return JSON.parse(error.message);
  } catch {
    throw new Error(`error is not generated RpcError JSON: ${error && error.message}`);
  }
}

function assertFrame(frame, label) {
  if (!frame || typeof frame !== "object") {
    throw new Error(`${label} must be a RuntimeWatchFrame object`);
  }
  if (!Array.isArray(frame.volumes) || frame.volumes.length === 0) {
    throw new Error(`${label} must carry replicated Docker Volumes`);
  }
  if (!frame.incomplete_ids || !Array.isArray(frame.incomplete_ids.containers)) {
    throw new Error(`${label} must carry typed incomplete IDs`);
  }
  if (!frame.containers[0]?.resolved_spec || !frame.services[0]?.containers[0]?.resolved_spec) {
    throw new Error(`${label} must reconstruct rich Container and Service views`);
  }
  if (frame.specs || frame.containers[0].spec_index != null || frame.services[0].member_ids) {
    throw new Error(`${label} leaked the normalized transport graph`);
  }
  const text = JSON.stringify(frame);
  if (Buffer.byteLength(text) <= 4 * 1024 * 1024) {
    throw new Error(`${label} must exceed tonic's default decoding limit`);
  }
  for (const forbidden of [
    "BEGIN CERTIFICATE",
    "BEGIN PRIVATE KEY",
    "private_key",
    "challenge_token",
    "challenge_response",
    "renewal_token",
    "dns_endpoint",
  ]) {
    if (text.includes(forbidden)) {
      throw new Error(`${label} leaked ${forbidden}`);
    }
  }
}

async function takeOne(client) {
  const ac = new AbortController();
  const frames = [];
  for await (const frame of client.runtime.watch({ signal: ac.signal })) {
    frames.push(frame);
    ac.abort();
  }
  if (frames.length !== 1) {
    throw new Error(`expected one Watch frame, got ${frames.length}`);
  }
  return frames[0];
}

(async () => {
  const client = await sdk.connect({ relayUrl, bearer, pairing, machineId });
  const first = await takeOne(client);
  assertFrame(first, "first Watch");
  const about = await client.about();
  if (!about.capabilities.includes("ployz.rpc.describe-contract.v1")) {
    throw new Error("about() must still work after AbortSignal ends Watch");
  }
  const second = await takeOne(client);
  assertFrame(second, "reconnect Watch");
  await client.close();
  try {
    await takeOne(client);
    throw new Error("Watch after close must fail");
  } catch (error) {
    const rpc = parseRpc(error);
    if (rpc.code !== "unavailable") {
      throw new Error(`expected unavailable after close, got ${rpc.code}: ${rpc.message}`);
    }
  }
  console.log("ok");
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
