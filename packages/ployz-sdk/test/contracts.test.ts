import assert from "node:assert/strict";
import test from "node:test";

import { API_MAJOR, KNOWN_API_FEATURES, machineName, routeHostname, routePort } from "../src/index.ts";

test("v2 constants and public primitives match their Rust constraints", () => {
  assert.equal(API_MAJOR, 1);
  assert.ok(KNOWN_API_FEATURES.includes("v2.deploy"));
  assert.equal(machineName("edge_1"), "edge_1");
  assert.equal(routeHostname("API.Example.com"), "api.example.com");
  assert.equal(routePort(443), 443);

  assert.throws(() => machineName("bad.name"), RangeError);
  assert.throws(() => routeHostname("-api.example.com"), RangeError);
  assert.throws(() => routePort(0), RangeError);
});
