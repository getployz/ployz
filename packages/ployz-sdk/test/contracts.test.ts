import assert from "node:assert/strict";
import test from "node:test";

import {
  API_MAJOR,
  KNOWN_API_FEATURES,
  imageReference,
  machineName,
  routeHostname,
  routePort,
} from "../src/index.ts";

test("generated contract exposes the V2 API", () => {
  assert.equal(API_MAJOR, 1);
  assert.ok(KNOWN_API_FEATURES.includes("v2.deploy"));
  assert.ok(KNOWN_API_FEATURES.includes("v2.operation_evidence"));
});

test("live primitive helpers enforce the Rust wire constraints", () => {
  assert.equal(machineName("worker_1"), "worker_1");
  assert.equal(imageReference("registry.example/app:latest"), "registry.example/app:latest");
  assert.equal(routeHostname("App.Example.com"), "app.example.com");
  assert.equal(routePort(8080), 8080);

  assert.throws(() => machineName("bad.name"), RangeError);
  assert.throws(() => imageReference("bad image"), RangeError);
  assert.throws(() => routeHostname("-bad.example.com"), RangeError);
  assert.throws(() => routePort(0), RangeError);
});
