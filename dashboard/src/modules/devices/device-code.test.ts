import { expect, it } from "vitest";
import { deviceCodeOf } from "#/modules/devices/device-code";

it("computes a Device's code as the Engine does", () => {
  expect(deviceCodeOf("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f")).toBe("mmg42klgyq");
});
